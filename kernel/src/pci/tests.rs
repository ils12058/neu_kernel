use super::*;
use std::collections::BTreeMap;

#[derive(Default)]
pub(crate) struct Mock {
    pub values: BTreeMap<(Bdf, u16), u32>,
    pub masks: BTreeMap<(Bdf, u16), u32>,
    pub writes: std::vec::Vec<(Bdf, u16, u32, u8)>,
}
impl Mock {
    pub fn endpoint(&mut self, bdf: Bdf) {
        self.values.insert((bdf, 0), 0x11e81234);
    }
    pub fn set(&mut self, bdf: Bdf, offset: u16, value: u32) {
        self.values.insert((bdf, offset), value);
    }
    fn bridge(&mut self, bdf: Bdf, secondary: u8, subordinate: u8) {
        self.endpoint(bdf);
        self.set(bdf, 8, 0x06040000);
        self.set(bdf, 0x0c, 1 << 16);
        self.set(
            bdf,
            0x18,
            u32::from(bdf.bus) | u32::from(secondary) << 8 | u32::from(subordinate) << 16,
        );
    }
}
impl Config for Mock {
    fn read32(&mut self, bdf: Bdf, offset: u16) -> Result<u32> {
        let value = *self
            .values
            .get(&(bdf, offset))
            .unwrap_or(&(if offset == 0 { u32::MAX } else { 0 }));
        Ok(if value == u32::MAX && (0x10..=0x24).contains(&offset) {
            *self.masks.get(&(bdf, offset)).unwrap_or(&0)
        } else {
            value
        })
    }
    fn write32(&mut self, bdf: Bdf, offset: u16, value: u32) -> Result<()> {
        self.writes.push((bdf, offset, value, 4));
        self.set(bdf, offset, value);
        Ok(())
    }
    fn write16(&mut self, bdf: Bdf, offset: u16, value: u16) -> Result<()> {
        assert_eq!(offset & 1, 0);
        let shift = (offset & 2) * 8;
        let old = self.read32(bdf, offset & !3)?;
        self.writes.push((bdf, offset, u32::from(value), 2));
        self.set(
            bdf,
            offset & !3,
            old & !(0xffff << shift) | u32::from(value) << shift,
        );
        Ok(())
    }
}
pub(crate) fn region() -> Ecam {
    Ecam {
        base: 0xe0000000,
        segment: 0,
        first: 0,
        last: 3,
    }
}
pub(crate) fn bdf(bus: u8, devfn: u8) -> Bdf {
    Bdf {
        segment: 0,
        bus,
        devfn,
    }
}

#[test]
fn ecam_checks_segments_width_overflow_and_nonzero_start_bus() {
    let r = Ecam {
        first: 2,
        ..region()
    };
    assert_eq!(r.address(bdf(2, 0x29), 0xffc, 4), Ok(0xe0229ffc));
    for (b, off, width) in [
        (bdf(1, 0), 0, 4),
        (
            Bdf {
                segment: 1,
                ..bdf(2, 0)
            },
            0,
            4,
        ),
        (bdf(2, 0), 4096, 4),
        (bdf(2, 0), 1, 4),
        (bdf(2, 0), 0, 8),
    ] {
        assert_eq!(r.address(b, off, width), Err(Error::Range));
    }
    assert!(
        Ecam {
            base: u64::MAX & !0xfffff,
            ..r
        }
        .validate()
        .is_err()
    );
    assert!(Ecam { base: 1, ..r }.validate().is_err());
}
#[test]
fn ecam_rejects_duplicate_bus_ranges_and_physical_aliases() {
    assert_eq!(
        validate_regions(&[region(), region()]),
        Err(Error::Conflict)
    );
    assert_eq!(
        validate_regions(&[
            region(),
            Ecam {
                segment: 1,
                ..region()
            }
        ]),
        Err(Error::Conflict)
    );
    assert!(
        validate_regions(&[
            region(),
            Ecam {
                base: 0xf0000000,
                segment: 1,
                ..region()
            }
        ])
        .is_ok()
    );
}
#[test]
fn discovers_multifunction_secondary_bus_and_swizzles_to_root() {
    let mut c = Mock::default();
    c.bridge(bdf(0, 0x10), 2, 3);
    c.endpoint(bdf(2, 0x28));
    c.set(bdf(2, 0x28), 0x0c, 1 << 23);
    c.endpoint(bdf(2, 0x29));
    let inv = Inventory::enumerate(&mut c, &[region()]).unwrap();
    assert_eq!(inv.functions().len(), 3);
    assert_eq!(inv.root_interrupt(bdf(2, 0x29), 1), Ok((bdf(0, 0x10), 2)));
    assert!(c.writes.is_empty());
}
#[test]
fn rejects_overlapping_bridge_windows_and_invalid_secondary_bus() {
    let mut c = Mock::default();
    c.bridge(bdf(0, 0x10), 1, 2);
    c.bridge(bdf(0, 0x18), 2, 3);
    assert!(matches!(
        Inventory::enumerate(&mut c, &[region()]),
        Err(Error::Conflict)
    ));
    c.set(bdf(0, 0x18), 0x18, 0);
    assert!(matches!(
        Inventory::enumerate(&mut c, &[region()]),
        Err(Error::Malformed)
    ));
}
#[test]
fn rejects_standard_capability_cycles_duplicates_and_misalignment() {
    for value in [0x4405, 0x4105] {
        let mut c = Mock::default();
        c.endpoint(bdf(0, 8));
        c.set(bdf(0, 8), 4, 1 << 20);
        c.set(bdf(0, 8), 0x34, 0x40);
        c.set(bdf(0, 8), 0x40, value);
        c.set(bdf(0, 8), 0x44, 0x4001);
        assert!(Inventory::enumerate(&mut c, &[region()]).is_err());
    }
    let mut c = Mock::default();
    c.endpoint(bdf(0, 8));
    c.set(bdf(0, 8), 4, 1 << 20);
    c.set(bdf(0, 8), 0x34, 0x40);
    c.set(bdf(0, 8), 0x40, 0x4405);
    c.set(bdf(0, 8), 0x44, 5);
    assert!(matches!(
        Inventory::enumerate(&mut c, &[region()]),
        Err(Error::Conflict)
    ));
}
#[test]
fn validates_pcie_extended_capability_chain() {
    let mut c = Mock::default();
    let b = bdf(0, 8);
    c.endpoint(b);
    c.set(b, 4, 1 << 20);
    c.set(b, 0x34, 0x40);
    c.set(b, 0x40, 0x10);
    c.set(b, 0x100, 1 | 1 << 16 | 0x140 << 20);
    c.set(b, 0x140, 3 | 1 << 16);
    assert_eq!(
        Inventory::enumerate(&mut c, &[region()])
            .unwrap()
            .get(b)
            .unwrap()
            .extended_count,
        2
    );
    c.set(b, 0x140, 3 | 1 << 16 | 0x100 << 20);
    assert!(matches!(
        Inventory::enumerate(&mut c, &[region()]),
        Err(Error::Malformed)
    ));
}
#[test]
fn probes_64bit_and_io_bars_without_replaying_status_or_enabling_dma() {
    let mut c = Mock::default();
    let b = bdf(0, 8);
    c.endpoint(b);
    c.set(b, 4, 0xf9000007);
    c.set(b, 0x10, 4);
    c.set(b, 0x14, 2);
    c.set(b, 0x18, 0xc001);
    c.masks.insert((b, 0x10), 0xffff0004);
    c.masks.insert((b, 0x14), u32::MAX);
    c.masks.insert((b, 0x18), 0xffffff01);
    let f = *Inventory::enumerate(&mut c, &[region()])
        .unwrap()
        .get(b)
        .unwrap();
    let bars = probe_bars(&mut c, &f).unwrap();
    assert_eq!(bars[0].unwrap().base, 0x2_00000000);
    assert_eq!(bars[0].unwrap().size, 0x10000);
    assert_eq!(bars[2].unwrap().kind, BarKind::Io);
    assert_eq!(bars[2].unwrap().size, 256);
    assert_eq!(c.read32(b, 4), Ok(0xf9000400));
    assert!(
        c.writes
            .iter()
            .filter(|(_, off, _, _)| *off == 4)
            .all(|(_, _, _, width)| *width == 2)
    );
    assert_eq!(c.read32(b, 0x10), Ok(4));
    assert_eq!(c.read32(b, 0x14), Ok(2));
}
#[test]
fn rejects_unassigned_misaligned_overlapping_and_overflowing_bars() {
    for (base, size) in [(0, 4096), (4097, 4096), (u64::MAX & !4095, 4096), (4096, 3)] {
        assert!(
            Bar {
                index: 0,
                kind: BarKind::Memory64,
                base,
                size,
                prefetch: false
            }
            .end()
            .is_err()
        );
    }
    let mut c = Mock::default();
    let b = bdf(0, 8);
    c.endpoint(b);
    for offset in [0x10, 0x14] {
        c.set(b, offset, 0x80000000);
        c.masks.insert((b, offset), 0xfffff000);
    }
    let f = *Inventory::enumerate(&mut c, &[region()])
        .unwrap()
        .get(b)
        .unwrap();
    assert_eq!(probe_bars(&mut c, &f), Err(Error::Conflict));
    assert_eq!(c.read32(b, 0x14), Ok(0x80000000));
}

#[test]
fn resource_conflicts_cross_function_and_memory_width_boundaries() {
    let memory = Bar {
        index: 0,
        kind: BarKind::Memory32,
        base: 0x80000000,
        size: 0x100000,
        prefetch: false,
    };
    assert_eq!(
        memory.conflicts(Bar {
            kind: BarKind::Memory64,
            base: 0x80080000,
            size: 0x80000,
            ..memory
        }),
        Ok(true)
    );
    assert_eq!(
        memory.conflicts(Bar {
            base: 0x80100000,
            ..memory
        }),
        Ok(false)
    );
    assert_eq!(
        memory.conflicts(Bar {
            kind: BarKind::Io,
            ..memory
        }),
        Ok(false)
    );
    assert!(
        memory
            .conflicts(Bar {
                base: u64::MAX - 4095,
                size: 4096,
                ..memory
            })
            .is_err()
    );
}
