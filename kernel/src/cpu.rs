//! CPU topology, bootstrap-state and per-CPU resource rules shared by the
//! host tests and the x86-64 SMP backend. This module contains no CPU
//! instructions and no shared mutable state.

pub const MAX_CPUS: usize = 4;
pub const XAPIC_ID_LIMIT: u32 = 256;

/// Fixed physical-address-free witness shared with the QMP observer.
pub const BLOCK_MAGIC: u64 = 0x4e45_555f_4350_5531; // "NEU_CPU1"
/// One 64-bit word per documented field; CPUs are `STRIDE` bytes apart.
pub const BLOCK_FIELDS: usize = 48;
pub const BLOCK_STRIDE: usize = BLOCK_FIELDS * 8;

/// Documented per-CPU block field indices. The x86-64 backend stores each
/// field in a target atomic, and the QMP observer reads the same offsets.
pub mod block {
    pub const MAGIC: usize = 0;
    pub const LOGICAL: usize = 1;
    pub const IS_BSP: usize = 2;
    pub const APIC_ID: usize = 3;
    pub const PROCESSOR_ID: usize = 4;
    pub const CPUID_APIC: usize = 5;
    pub const STATE: usize = 6;
    pub const HISTORY: usize = 7;
    pub const ENTRY_COUNT: usize = 8;
    pub const STACK_BOTTOM: usize = 9;
    pub const STACK_TOP: usize = 10;
    pub const ENTRY_RSP: usize = 11;
    pub const IST_BOTTOM: usize = 12;
    pub const IST_TOP: usize = 13;
    pub const GDT_BASE: usize = 14;
    pub const GDT_LIMIT: usize = 15;
    pub const TSS_BASE: usize = 16;
    pub const TSS_LIMIT: usize = 17;
    pub const IDT_BASE: usize = 18;
    pub const IDT_LIMIT: usize = 19;
    pub const CR0: usize = 20;
    pub const CR3: usize = 21;
    pub const CR4: usize = 22;
    pub const EFER: usize = 23;
    pub const RFLAGS: usize = 24;
    pub const APIC_BASE: usize = 25;
    pub const GS_BASE: usize = 26;
    pub const READY_RSP: usize = 27;
    pub const PARK_RSP: usize = 28;
    pub const PARK_COUNT: usize = 29;
    pub const CANARY: usize = 30;
    pub const OBSERVED_CANARY: usize = 31;
    pub const SELF_TEST: usize = 32;
    pub const FAULT_VECTOR: usize = 33;
    pub const FAULT_ERROR: usize = 34;
    pub const FAULT_RIP: usize = 35;
    pub const LOADER_CR3: usize = 36;
    pub const LOADER_RSP: usize = 37;
    pub const ISOLATION: usize = 38;
    pub const SCHEDULER_LOCAL: usize = 39;
    pub const MP_INFO: usize = 40;
    pub const RELEASE_RIP: usize = 41;
    pub const CANARY_IST: usize = 42;
    pub const OBSERVED_IST_CANARY: usize = 43;
    pub const WAIT_SPINS: usize = 44;
    pub const FAILURE: usize = 45;
    pub const EXPECTED_APIC_BASE: usize = 46;
    pub const RESERVED: usize = 47;

    pub const CANARY_VALUE: u64 = 0x4e45_555f_4150_5354; // "NEU_APST"

    /// Each bit records a check the AP performed on its own architectural
    /// state before publishing Ready.
    pub const SELF_TEST_STACK: u64 = 1 << 0;
    pub const SELF_TEST_IST: u64 = 1 << 1;
    pub const SELF_TEST_GDT: u64 = 1 << 2;
    pub const SELF_TEST_IDT: u64 = 1 << 3;
    pub const SELF_TEST_GS: u64 = 1 << 4;
    pub const SELF_TEST_ROOT: u64 = 1 << 5;
    pub const SELF_TEST_PRIVILEGE: u64 = 1 << 6;
    pub const SELF_TEST_APIC: u64 = 1 << 7;
    pub const SELF_TEST_IDENTITY: u64 = 1 << 8;
    pub const SELF_TEST_ALL: u64 = (1 << 9) - 1;

    /// AP isolation claims verified independently by the BSP and QMP.
    pub const ISOLATION_NO_USER: u64 = 1 << 0;
    pub const ISOLATION_NO_APIC_TIMER: u64 = 1 << 1;
    pub const ISOLATION_NO_FOREIGN_STACK: u64 = 1 << 2;
    pub const ISOLATION_NO_SUBSYSTEM: u64 = 1 << 3;
    pub const ISOLATION_IF_DISABLED: u64 = 1 << 4;
    pub const ISOLATION_PRIVATE_TABLES: u64 = 1 << 5;
    pub const ISOLATION_AP: u64 = (1 << 6) - 1;
    /// The BSP legitimately enters paging and the boot subsystems.
    pub const ISOLATION_BSP: u64 = ISOLATION_AP & !ISOLATION_NO_SUBSYSTEM;
}

/// Fixed minimum sizes for the BSP and application-processor bootstrap stacks.
pub const BSP_STACK_MINIMUM: u64 = 64 * 1024;
pub const BSP_IST_MINIMUM: u64 = 32 * 1024;
pub const AP_STACK_MINIMUM: u64 = 16 * 1024;
pub const AP_IST_MINIMUM: u64 = 8 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Processor {
    pub apic_id: u32,
    pub processor_id: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TopologyError {
    Empty,
    Expected,
    Capacity,
    DuplicateApic,
    DuplicateProcessor,
    MissingBsp,
    ApicRange,
    BspIdentity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Assignment {
    pub logical: usize,
    pub processor: Processor,
}

/// Logical indices are dense and independent of hardware APIC identities.
/// The BSP is logical 0; APs are ordered by ascending APIC ID so the mapping
/// does not depend on bootloader release order. Every AP keeps its sparsely
/// numbered hardware identities in `processor`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Topology {
    entries: [Option<Assignment>; MAX_CPUS],
    count: usize,
}

impl Topology {
    pub fn discover(
        bsp_apic_id: u32,
        bsp_cpuid_apic_id: u32,
        processors: &[Processor],
        expected: usize,
    ) -> Result<Self, TopologyError> {
        if processors.is_empty() {
            return Err(TopologyError::Empty);
        }
        if expected == 0 || expected > MAX_CPUS || processors.len() > MAX_CPUS {
            return Err(TopologyError::Capacity);
        }
        if processors.len() != expected {
            return Err(TopologyError::Expected);
        }
        if bsp_apic_id != bsp_cpuid_apic_id {
            return Err(TopologyError::BspIdentity);
        }
        let mut entries = [None; MAX_CPUS];
        let mut bsp = None;
        for (index, processor) in processors.iter().copied().enumerate() {
            if processor.apic_id >= XAPIC_ID_LIMIT {
                return Err(TopologyError::ApicRange);
            }
            if processors[..index]
                .iter()
                .any(|seen| seen.apic_id == processor.apic_id)
            {
                return Err(TopologyError::DuplicateApic);
            }
            if processors[..index]
                .iter()
                .any(|seen| seen.processor_id == processor.processor_id)
            {
                return Err(TopologyError::DuplicateProcessor);
            }
            if processor.apic_id == bsp_apic_id {
                bsp = Some(Assignment {
                    logical: 0,
                    processor,
                });
            }
        }
        let bsp = bsp.ok_or(TopologyError::MissingBsp)?;
        entries[0] = Some(bsp);
        let mut aps = [None; MAX_CPUS];
        let mut ap_count = 0;
        for processor in processors.iter().copied() {
            if processor.apic_id != bsp_apic_id {
                aps[ap_count] = Some(processor);
                ap_count += 1;
            }
        }
        for i in 1..ap_count {
            let mut j = i;
            while j > 0
                && aps[j - 1].is_some_and(|previous| {
                    aps[j].is_some_and(|current| previous.apic_id > current.apic_id)
                })
            {
                aps.swap(j - 1, j);
                j -= 1;
            }
        }
        for logical in 1..processors.len() {
            entries[logical] = Some(Assignment {
                logical,
                processor: aps[logical - 1].ok_or(TopologyError::MissingBsp)?,
            });
        }
        Ok(Self {
            entries,
            count: processors.len(),
        })
    }

    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn get(&self, logical: usize) -> Option<Assignment> {
        (logical < self.count)
            .then(|| self.entries[logical])
            .flatten()
    }

    pub fn logical_for_apic(&self, apic_id: u32) -> Option<usize> {
        self.entries[..self.count]
            .iter()
            .flatten()
            .find(|assignment| assignment.processor.apic_id == apic_id)
            .map(|assignment| assignment.logical)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bootstrap {
    Uninitialized = 0,
    Prepared = 1,
    Entered = 2,
    Ready = 3,
    Parked = 4,
    Failed = 5,
}

impl Bootstrap {
    pub const fn code(self) -> u64 {
        self as u64
    }

    pub const fn from_code(code: u64) -> Option<Self> {
        Some(match code {
            0 => Self::Uninitialized,
            1 => Self::Prepared,
            2 => Self::Entered,
            3 => Self::Ready,
            4 => Self::Parked,
            5 => Self::Failed,
            _ => return None,
        })
    }

    pub const fn bit(self) -> u64 {
        1 << self.code()
    }

    pub const fn next(self) -> Option<Self> {
        Some(match self {
            Self::Uninitialized => Self::Prepared,
            Self::Prepared => Self::Entered,
            Self::Entered => Self::Ready,
            Self::Ready => Self::Parked,
            Self::Parked | Self::Failed => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StateError {
    Order,
}

/// Host-testable view of the AP state machine. Hardware publishes the same
/// state/history pair through atomics, one writer per CPU.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    state: Bootstrap,
    history: u64,
}

impl Default for Progress {
    fn default() -> Self {
        Self::new()
    }
}

impl Progress {
    pub const fn new() -> Self {
        Self {
            state: Bootstrap::Uninitialized,
            history: Bootstrap::Uninitialized.bit(),
        }
    }

    pub fn state(&self) -> Bootstrap {
        self.state
    }

    pub fn history(&self) -> u64 {
        self.history
    }

    /// Rebuilds the host-tested state machine around an atomic publication.
    /// A history that omits the current state is not a valid publication.
    pub fn from_parts(state: Bootstrap, history: u64) -> Option<Self> {
        (history & state.bit() != 0).then_some(Self { state, history })
    }

    pub fn advance(&mut self, next: Bootstrap) -> Result<(), StateError> {
        if next == Bootstrap::Failed {
            self.state = Bootstrap::Failed;
            self.history |= Bootstrap::Failed.bit();
            return Ok(());
        }
        if self.state.next() != Some(next) {
            return Err(StateError::Order);
        }
        self.state = next;
        self.history |= next.bit();
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wait {
    Pending,
    Complete,
    Failed,
}

/// A parked AP must prove every intermediate transition, not just the final
/// state. `state` and `history` come from the same per-CPU block.
pub fn ap_wait(state: Bootstrap, history: u64) -> Wait {
    if state == Bootstrap::Failed {
        return Wait::Failed;
    }
    if state != Bootstrap::Parked {
        return Wait::Pending;
    }
    let required = Bootstrap::Prepared.bit()
        | Bootstrap::Entered.bit()
        | Bootstrap::Ready.bit()
        | Bootstrap::Parked.bit();
    if history & required == required {
        Wait::Complete
    } else {
        Wait::Failed
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub bottom: u64,
    pub top: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutError {
    Malformed,
    Overlap,
}

impl Region {
    pub fn new(bottom: u64, top: u64, minimum: u64, align: u64) -> Option<Self> {
        if bottom >= top
            || top - bottom < minimum
            || align == 0
            || !align.is_power_of_two()
            || !bottom.is_multiple_of(align)
            || !top.is_multiple_of(align)
        {
            return None;
        }
        Some(Self { bottom, top })
    }

    pub fn contains(self, value: u64) -> bool {
        self.bottom <= value && value < self.top
    }

    pub fn overlaps(self, other: Self) -> bool {
        self.bottom < other.top && other.bottom < self.top
    }
}

/// Every per-CPU stack and IST stack must be separate from every other one.
pub fn validate_regions(regions: &[Region]) -> Result<(), LayoutError> {
    for (index, region) in regions.iter().enumerate() {
        if region.bottom >= region.top {
            return Err(LayoutError::Malformed);
        }
        if regions[..index]
            .iter()
            .any(|previous| previous.overlaps(*region))
        {
            return Err(LayoutError::Overlap);
        }
    }
    Ok(())
}

/// Validates the complete BSP/AP stack layout: each region must meet its
/// documented minimum and no two regions may overlap. Guard pages are
/// validated separately because they are adjacent, not overlapping, pages.
pub fn validate_cpu_regions(regions: &[Region], minimums: &[u64]) -> Result<(), LayoutError> {
    if regions.len() != minimums.len() {
        return Err(LayoutError::Malformed);
    }
    for (region, minimum) in regions.iter().zip(minimums) {
        if region.top - region.bottom < *minimum {
            return Err(LayoutError::Malformed);
        }
    }
    validate_regions(regions)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn processor(apic_id: u32, processor_id: u32) -> Processor {
        Processor {
            apic_id,
            processor_id,
        }
    }

    #[test]
    fn single_processor_topology_assigns_the_bsp_to_logical_zero() {
        let topology = Topology::discover(0, 0, &[processor(0, 7)], 1).unwrap();
        assert_eq!(topology.len(), 1);
        assert_eq!(topology.get(0).unwrap().processor.apic_id, 0);
        assert_eq!(topology.get(0).unwrap().processor.processor_id, 7);
        assert!(topology.get(1).is_none());
        assert_eq!(topology.logical_for_apic(0), Some(0));
    }

    #[test]
    fn logical_indices_are_dense_while_apic_ids_remain_sparse() {
        let topology = Topology::discover(
            7,
            7,
            &[
                processor(42, 9),
                processor(7, 2),
                processor(3, 4),
                processor(9, 6),
            ],
            4,
        )
        .unwrap();
        let ids: [u32; 4] =
            [0, 1, 2, 3].map(|logical| topology.get(logical).unwrap().processor.apic_id);
        assert_eq!(ids, [7, 3, 9, 42]);
        assert_eq!(topology.logical_for_apic(3), Some(1));
        assert_eq!(topology.logical_for_apic(9), Some(2));
        assert_eq!(topology.logical_for_apic(42), Some(3));
        assert_eq!(topology.logical_for_apic(7), Some(0));
    }

    #[test]
    fn two_processor_topology_keeps_the_bsp_first() {
        let topology = Topology::discover(9, 9, &[processor(9, 1), processor(2, 0)], 2).unwrap();
        assert_eq!(topology.get(0).unwrap().processor.apic_id, 9);
        assert_eq!(topology.get(1).unwrap().processor.apic_id, 2);
    }

    #[test]
    fn duplicate_apic_and_processor_identities_are_rejected() {
        assert_eq!(
            Topology::discover(0, 0, &[processor(0, 1), processor(0, 2)], 2),
            Err(TopologyError::DuplicateApic)
        );
        assert_eq!(
            Topology::discover(0, 0, &[processor(0, 1), processor(1, 1)], 2),
            Err(TopologyError::DuplicateProcessor)
        );
    }

    #[test]
    fn missing_mismatched_or_oversized_bsp_descriptions_are_rejected() {
        assert_eq!(
            Topology::discover(0, 0, &[processor(1, 0)], 1),
            Err(TopologyError::MissingBsp)
        );
        assert_eq!(
            Topology::discover(0, 3, &[processor(0, 0)], 1),
            Err(TopologyError::BspIdentity)
        );
        assert_eq!(Topology::discover(0, 0, &[], 1), Err(TopologyError::Empty));
        assert_eq!(
            Topology::discover(0, 0, &[processor(0, 0)], 5),
            Err(TopologyError::Capacity)
        );
        assert_eq!(
            Topology::discover(0, 0, &[processor(0, 0)], 2),
            Err(TopologyError::Expected)
        );
        assert_eq!(
            Topology::discover(0, 0, &[processor(0, 0), processor(256, 1)], 2),
            Err(TopologyError::ApicRange)
        );
    }

    #[test]
    fn bootstrap_progress_accepts_exactly_the_documented_sequence() {
        let mut progress = Progress::new();
        assert_eq!(progress.state(), Bootstrap::Uninitialized);
        progress.advance(Bootstrap::Prepared).unwrap();
        progress.advance(Bootstrap::Entered).unwrap();
        progress.advance(Bootstrap::Ready).unwrap();
        progress.advance(Bootstrap::Parked).unwrap();
        assert_eq!(
            progress.history(),
            Bootstrap::Uninitialized.bit()
                | Bootstrap::Prepared.bit()
                | Bootstrap::Entered.bit()
                | Bootstrap::Ready.bit()
                | Bootstrap::Parked.bit()
        );
        assert_eq!(progress.advance(Bootstrap::Ready), Err(StateError::Order));
    }

    #[test]
    fn bootstrap_progress_rejects_skips_and_reverse_transitions() {
        let mut progress = Progress::new();
        assert_eq!(progress.advance(Bootstrap::Entered), Err(StateError::Order));
        progress.advance(Bootstrap::Prepared).unwrap();
        assert_eq!(progress.advance(Bootstrap::Parked), Err(StateError::Order));
        assert_eq!(
            progress.advance(Bootstrap::Uninitialized),
            Err(StateError::Order)
        );
    }

    #[test]
    fn failed_state_is_terminal_and_recorded() {
        let mut progress = Progress::new();
        progress.advance(Bootstrap::Prepared).unwrap();
        progress.advance(Bootstrap::Failed).unwrap();
        assert_eq!(progress.state(), Bootstrap::Failed);
        assert_ne!(progress.history() & Bootstrap::Failed.bit(), 0);
        assert_eq!(progress.advance(Bootstrap::Entered), Err(StateError::Order));
    }

    #[test]
    fn ap_wait_requires_a_complete_transition_history() {
        let complete = Bootstrap::Prepared.bit()
            | Bootstrap::Entered.bit()
            | Bootstrap::Ready.bit()
            | Bootstrap::Parked.bit();
        assert_eq!(ap_wait(Bootstrap::Ready, complete), Wait::Pending);
        assert_eq!(ap_wait(Bootstrap::Parked, complete), Wait::Complete);
        assert_eq!(
            ap_wait(Bootstrap::Parked, Bootstrap::Parked.bit()),
            Wait::Failed
        );
        assert_eq!(ap_wait(Bootstrap::Failed, 0), Wait::Failed);
    }

    #[test]
    fn stack_regions_must_be_aligned_sized_and_disjoint() {
        let bsp = Region::new(0x10_0000, 0x11_0000, 0x1_0000, 0x1000).unwrap();
        let ap1 = Region::new(0x20_0000, 0x20_4000, 0x4000, 0x1000).unwrap();
        let ap2 = Region::new(0x21_0000, 0x21_4000, 0x4000, 0x1000).unwrap();
        assert_eq!(validate_regions(&[bsp, ap1, ap2]), Ok(()));
        assert!(bsp.contains(0x10_0000));
        assert!(!bsp.contains(0x11_0000));
        assert_eq!(
            validate_regions(&[bsp, ap1, ap1]),
            Err(LayoutError::Overlap)
        );
        assert_eq!(
            validate_regions(&[
                bsp,
                Region {
                    bottom: 0x10_8000,
                    top: 0x11_8000
                }
            ]),
            Err(LayoutError::Overlap)
        );
        assert_eq!(
            validate_regions(&[Region {
                bottom: 0x30_0000,
                top: 0x30_0000
            }]),
            Err(LayoutError::Malformed)
        );
        assert!(Region::new(0x10_0000, 0x10_0fff, 0x1000, 0x1000).is_none());
        assert!(Region::new(0x10_0001, 0x10_1001, 0x1000, 0x1000).is_none());
    }

    #[test]
    fn per_cpu_regions_enforce_documented_minimums_and_disjointness() {
        let bsp = Region::new(0x10_0000, 0x11_0000, BSP_STACK_MINIMUM, 0x1000).unwrap();
        let ist = Region::new(0x12_0000, 0x12_8000, BSP_IST_MINIMUM, 0x1000).unwrap();
        let ap = Region::new(0x20_0000, 0x20_4000, AP_STACK_MINIMUM, 0x1000).unwrap();
        let ap_ist = Region::new(0x21_0000, 0x21_2000, AP_IST_MINIMUM, 0x1000).unwrap();
        assert_eq!(
            validate_cpu_regions(
                &[bsp, ist, ap, ap_ist],
                &[
                    BSP_STACK_MINIMUM,
                    BSP_IST_MINIMUM,
                    AP_STACK_MINIMUM,
                    AP_IST_MINIMUM
                ]
            ),
            Ok(())
        );
        assert_eq!(
            validate_cpu_regions(
                &[bsp, ist, ap, ap_ist],
                &[
                    BSP_STACK_MINIMUM,
                    BSP_IST_MINIMUM,
                    AP_STACK_MINIMUM + 1,
                    AP_IST_MINIMUM
                ]
            ),
            Err(LayoutError::Malformed)
        );
        assert_eq!(
            validate_cpu_regions(&[bsp, ist], &[BSP_STACK_MINIMUM]),
            Err(LayoutError::Malformed)
        );
        assert_eq!(
            validate_cpu_regions(
                &[bsp, ist, ap, ap],
                &[
                    BSP_STACK_MINIMUM,
                    BSP_IST_MINIMUM,
                    AP_STACK_MINIMUM,
                    AP_IST_MINIMUM
                ]
            ),
            Err(LayoutError::Overlap)
        );
        assert!(Region::new(0x20_0000, 0x20_2000, AP_STACK_MINIMUM, 0x1000).is_none());
    }

    #[test]
    fn cpu_block_layout_is_unique_contiguous_and_stable() {
        let indices = [
            block::MAGIC,
            block::LOGICAL,
            block::IS_BSP,
            block::APIC_ID,
            block::PROCESSOR_ID,
            block::CPUID_APIC,
            block::STATE,
            block::HISTORY,
            block::ENTRY_COUNT,
            block::STACK_BOTTOM,
            block::STACK_TOP,
            block::ENTRY_RSP,
            block::IST_BOTTOM,
            block::IST_TOP,
            block::GDT_BASE,
            block::GDT_LIMIT,
            block::TSS_BASE,
            block::TSS_LIMIT,
            block::IDT_BASE,
            block::IDT_LIMIT,
            block::CR0,
            block::CR3,
            block::CR4,
            block::EFER,
            block::RFLAGS,
            block::APIC_BASE,
            block::GS_BASE,
            block::READY_RSP,
            block::PARK_RSP,
            block::PARK_COUNT,
            block::CANARY,
            block::OBSERVED_CANARY,
            block::SELF_TEST,
            block::FAULT_VECTOR,
            block::FAULT_ERROR,
            block::FAULT_RIP,
            block::LOADER_CR3,
            block::LOADER_RSP,
            block::ISOLATION,
            block::SCHEDULER_LOCAL,
            block::MP_INFO,
            block::RELEASE_RIP,
            block::CANARY_IST,
            block::OBSERVED_IST_CANARY,
            block::WAIT_SPINS,
            block::FAILURE,
            block::EXPECTED_APIC_BASE,
            block::RESERVED,
        ];
        assert_eq!(BLOCK_FIELDS, indices.len());
        assert_eq!(BLOCK_STRIDE, indices.len() * 8);
        let mut sorted = indices;
        sorted.sort_unstable();
        assert!(sorted.iter().copied().eq(0..BLOCK_FIELDS));
    }

    #[test]
    fn progress_can_be_rebuilt_from_published_parts() {
        let complete = Bootstrap::Uninitialized.bit()
            | Bootstrap::Prepared.bit()
            | Bootstrap::Entered.bit()
            | Bootstrap::Ready.bit()
            | Bootstrap::Parked.bit();
        let mut progress = Progress::from_parts(Bootstrap::Ready, complete).unwrap();
        assert_eq!(progress.state(), Bootstrap::Ready);
        assert_eq!(progress.history(), complete);
        progress.advance(Bootstrap::Parked).unwrap();
        assert_eq!(progress.state(), Bootstrap::Parked);
        assert!(Progress::from_parts(Bootstrap::Ready, Bootstrap::Ready.bit()).is_some());
        assert!(Progress::from_parts(Bootstrap::Ready, Bootstrap::Prepared.bit()).is_none());
    }
}
