//! Bounded PCI inventory and resource validation, independent of configuration
//! transport. Discovery supplies facts; it never grants userspace authority.
pub const MAX_REGIONS: usize = 8;
pub const MAX_FUNCTIONS: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Absent,
    Range,
    Malformed,
    Capacity,
    Conflict,
    Unsupported,
    Access,
}
pub type Result<T> = core::result::Result<T, Error>;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Bdf {
    pub segment: u16,
    pub bus: u8,
    pub devfn: u8,
}
impl Bdf {
    pub const fn requester(self) -> u16 {
        ((self.bus as u16) << 8) | self.devfn as u16
    }
    pub const fn key(self) -> u64 {
        ((self.segment as u64) << 16) | self.requester() as u64
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ecam {
    pub base: u64,
    pub segment: u16,
    pub first: u8,
    pub last: u8,
}
impl Ecam {
    pub fn validate(self) -> Result<()> {
        if self.base == 0 || self.base & 0xfffff != 0 || self.first > self.last {
            return Err(Error::Range);
        }
        self.base
            .checked_add((u64::from(self.last) + 1) << 20)
            .ok_or(Error::Range)?;
        Ok(())
    }
    pub fn address(self, bdf: Bdf, offset: u16, width: u8) -> Result<u64> {
        self.validate()?;
        if bdf.segment != self.segment
            || !(self.first..=self.last).contains(&bdf.bus)
            || !matches!(width, 1 | 2 | 4)
            || !offset.is_multiple_of(u16::from(width))
            || u32::from(offset) + u32::from(width) > 4096
        {
            return Err(Error::Range);
        }
        // MCFG base denotes bus zero, including entries with nonzero start bus.
        self.base
            .checked_add(u64::from(bdf.requester()) << 12)
            .and_then(|v| v.checked_add(u64::from(offset)))
            .ok_or(Error::Range)
    }
}

pub fn validate_regions(regions: &[Ecam]) -> Result<()> {
    if regions.is_empty() || regions.len() > MAX_REGIONS {
        return Err(Error::Capacity);
    }
    for (i, a) in regions.iter().enumerate() {
        a.validate()?;
        let a0 = a.base + (u64::from(a.first) << 20);
        let a1 = a.base + ((u64::from(a.last) + 1) << 20);
        for b in &regions[..i] {
            let b0 = b.base + (u64::from(b.first) << 20);
            let b1 = b.base + ((u64::from(b.last) + 1) << 20);
            if (a.segment == b.segment && a.first <= b.last && b.first <= a.last)
                || (a0 < b1 && b0 < a1)
            {
                return Err(Error::Conflict);
            }
        }
    }
    Ok(())
}

pub trait Config {
    fn read32(&mut self, bdf: Bdf, offset: u16) -> Result<u32>;
    fn write32(&mut self, bdf: Bdf, offset: u16, value: u32) -> Result<()>;
    fn write16(&mut self, bdf: Bdf, offset: u16, value: u16) -> Result<()>;
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Function {
    pub bdf: Bdf,
    pub id: u32,
    pub class_revision: u32,
    pub command: u16,
    pub header: u8,
    pub pin: u8,
    pub primary: u8,
    pub secondary: u8,
    pub subordinate: u8,
    pub msi: u8,
    pub msix: u8,
    pub pcie: u8,
    pub capability_count: u8,
    pub extended_count: u8,
    pub bars: [u32; 6],
}
impl Function {
    fn read(c: &mut impl Config, bdf: Bdf, id: u32) -> Result<Self> {
        let command_status = c.read32(bdf, 4)?;
        let header = ((c.read32(bdf, 0x0c)? >> 16) & 0x7f) as u8;
        if header > 1 {
            return Err(Error::Unsupported);
        }
        let mut f = Self {
            bdf,
            id,
            class_revision: c.read32(bdf, 8)?,
            command: command_status as u16,
            header,
            pin: (c.read32(bdf, 0x3c)? >> 8) as u8,
            ..Self::default()
        };
        if f.pin > 4 {
            return Err(Error::Malformed);
        }
        for index in 0..if header == 0 { 6 } else { 2 } {
            f.bars[index] = c.read32(bdf, 0x10 + index as u16 * 4)?;
        }
        if header == 1 {
            let buses = c.read32(bdf, 0x18)?;
            f.primary = buses as u8;
            f.secondary = (buses >> 8) as u8;
            f.subordinate = (buses >> 16) as u8;
            if f.primary != bdf.bus
                || f.secondary == 0
                || f.secondary <= f.primary
                || f.secondary > f.subordinate
                || f.class_revision >> 16 != 0x0604
            {
                return Err(Error::Malformed);
            }
        }
        if command_status & (1 << 20) != 0 {
            let mut next = (c.read32(bdf, 0x34)? & 255) as u16;
            let mut seen = [false; 64];
            while next != 0 {
                if !(0x40..=0xfc).contains(&next) || next & 3 != 0 || seen[next as usize / 4] {
                    return Err(Error::Malformed);
                }
                seen[next as usize / 4] = true;
                let cap = c.read32(bdf, next)?;
                let slot = match cap & 255 {
                    5 => Some(&mut f.msi),
                    0x10 => Some(&mut f.pcie),
                    0x11 => Some(&mut f.msix),
                    _ => None,
                };
                if let Some(slot) = slot {
                    if *slot != 0 {
                        return Err(Error::Conflict);
                    }
                    *slot = next as u8;
                }
                f.capability_count += 1;
                next = ((cap >> 8) & 255) as u16;
            }
        }
        if f.pcie != 0 {
            let mut seen = [false; 1024];
            let mut next = 0x100u16;
            while next != 0 {
                if !(0x100..=0xffc).contains(&next) || next & 3 != 0 || seen[next as usize / 4] {
                    return Err(Error::Malformed);
                }
                seen[next as usize / 4] = true;
                let cap = c.read32(bdf, next)?;
                if cap == 0 || cap == u32::MAX {
                    break;
                }
                if cap & 0xffff == 0 || (cap >> 16) & 15 == 0 || f.extended_count == 64 {
                    return Err(Error::Malformed);
                }
                f.extended_count += 1;
                next = (cap >> 20) as u16;
            }
        }
        Ok(f)
    }
}

pub struct Inventory {
    functions: [Function; MAX_FUNCTIONS],
    len: usize,
}
impl Default for Inventory {
    fn default() -> Self {
        Self::new()
    }
}
impl Inventory {
    pub const fn new() -> Self {
        Self {
            functions: [Function {
                bdf: Bdf {
                    segment: 0,
                    bus: 0,
                    devfn: 0,
                },
                id: 0,
                class_revision: 0,
                command: 0,
                header: 0,
                pin: 0,
                primary: 0,
                secondary: 0,
                subordinate: 0,
                msi: 0,
                msix: 0,
                pcie: 0,
                capability_count: 0,
                extended_count: 0,
                bars: [0; 6],
            }; MAX_FUNCTIONS],
            len: 0,
        }
    }
    pub fn functions(&self) -> &[Function] {
        &self.functions[..self.len]
    }
    pub fn get(&self, bdf: Bdf) -> Result<&Function> {
        self.functions()
            .iter()
            .find(|f| f.bdf == bdf)
            .ok_or(Error::Absent)
    }
    pub fn enumerate(c: &mut impl Config, regions: &[Ecam]) -> Result<Self> {
        validate_regions(regions)?;
        let mut result = Self::new();
        for region in regions {
            // Cover all firmware-advertised buses, including additional host
            // roots that cannot be inferred from one PCI-to-PCI bridge tree.
            for bus in region.first..=region.last {
                for device in 0..32 {
                    let base = Bdf {
                        segment: region.segment,
                        bus,
                        devfn: device << 3,
                    };
                    let id = c.read32(base, 0)?;
                    if id as u16 == 0xffff {
                        continue;
                    }
                    let count = if c.read32(base, 0x0c)? & (1 << 23) != 0 {
                        8
                    } else {
                        1
                    };
                    for function in 0..count {
                        let bdf = Bdf {
                            devfn: base.devfn + function,
                            ..base
                        };
                        let id = if function == 0 { id } else { c.read32(bdf, 0)? };
                        if id as u16 == 0xffff {
                            continue;
                        }
                        if result.len == MAX_FUNCTIONS {
                            return Err(Error::Capacity);
                        }
                        let f = Function::read(c, bdf, id)?;
                        if f.header == 1 && f.subordinate > region.last {
                            return Err(Error::Range);
                        }
                        result.functions[result.len] = f;
                        result.len += 1;
                    }
                }
            }
        }
        for (i, a) in result
            .functions()
            .iter()
            .enumerate()
            .filter(|(_, f)| f.header == 1)
        {
            for b in result.functions()[..i]
                .iter()
                .filter(|f| f.header == 1 && f.bdf.segment == a.bdf.segment)
            {
                let overlap = a.secondary <= b.subordinate && b.secondary <= a.subordinate;
                let nested = (a.secondary <= b.primary && b.subordinate <= a.subordinate)
                    || (b.secondary <= a.primary && a.subordinate <= b.subordinate);
                if overlap && !nested {
                    return Err(Error::Conflict);
                }
            }
        }
        Ok(result)
    }
    pub fn parent(&self, bdf: Bdf) -> Option<&Function> {
        self.functions()
            .iter()
            .filter(|f| {
                f.header == 1
                    && f.bdf.segment == bdf.segment
                    && (f.secondary..=f.subordinate).contains(&bdf.bus)
            })
            .min_by_key(|f| u16::from(f.subordinate) - u16::from(f.secondary))
    }
    /// Standard PCI bridge swizzling, terminating at a platform root bus.
    /// Platform policy then translates the root slot/pin to a controller GSI.
    pub fn root_interrupt(&self, mut bdf: Bdf, mut pin: u8) -> Result<(Bdf, u8)> {
        if !(1..=4).contains(&pin) {
            return Err(Error::Range);
        }
        for _ in 0..256 {
            match self.parent(bdf) {
                None => return Ok((bdf, pin)),
                Some(bridge) => {
                    pin = ((pin - 1 + (bdf.devfn >> 3)) % 4) + 1;
                    bdf = bridge.bdf;
                }
            }
        }
        Err(Error::Malformed)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarKind {
    Io,
    Memory32,
    Memory64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bar {
    pub index: u8,
    pub kind: BarKind,
    pub base: u64,
    pub size: u64,
    pub prefetch: bool,
}
impl Bar {
    pub fn end(self) -> Result<u64> {
        if self.base == 0
            || self.size == 0
            || !self.size.is_power_of_two()
            || !self.base.is_multiple_of(self.size)
        {
            return Err(Error::Range);
        }
        self.base.checked_add(self.size).ok_or(Error::Range)
    }
    /// I/O space and memory space are separate. 32/64-bit memory BARs share
    /// the same physical resource namespace, including across PCI functions.
    pub fn conflicts(self, other: Self) -> Result<bool> {
        let end = self.end()?;
        let other_end = other.end()?;
        Ok((self.kind == BarKind::Io) == (other.kind == BarKind::Io)
            && self.base < other_end
            && other.base < end)
    }
}

/// Probe only a selected, exclusively managed function. Memory/I/O decode and
/// bus mastering remain disabled on return, including every failure path.
/// Command halfword writes never replay W1C PCI status bits.
pub fn probe_bars(c: &mut impl Config, f: &Function) -> Result<[Option<Bar>; 6]> {
    c.write16(f.bdf, 4, (f.command & !7) | 0x400)?;
    let mut result: [Option<Bar>; 6] = [None; 6];
    let count = if f.header == 0 { 6 } else { 2 };
    let mut index = 0;
    while index < count {
        let offset = 0x10 + index as u16 * 4;
        let low = c.read32(f.bdf, offset)?;
        let kind = if low & 1 != 0 {
            BarKind::Io
        } else {
            match low & 6 {
                0 => BarKind::Memory32,
                4 if index + 1 < count => BarKind::Memory64,
                _ => return Err(Error::Unsupported),
            }
        };
        let high = if kind == BarKind::Memory64 {
            c.read32(f.bdf, offset + 4)?
        } else {
            0
        };
        c.write32(f.bdf, offset, u32::MAX)?;
        let mask = (|| {
            if kind == BarKind::Memory64 {
                c.write32(f.bdf, offset + 4, u32::MAX)?;
            }
            let lo = c.read32(f.bdf, offset)?;
            let hi = if kind == BarKind::Memory64 {
                c.read32(f.bdf, offset + 4)?
            } else {
                0
            };
            Ok((lo, hi))
        })();
        let restored_low = c.write32(f.bdf, offset, low);
        let restored_high = if kind == BarKind::Memory64 {
            c.write32(f.bdf, offset + 4, high)
        } else {
            Ok(())
        };
        restored_low?;
        restored_high?;
        let (mask_low, mask_high) = mask?;
        let address_mask = if kind == BarKind::Io { !3 } else { !15 };
        if mask_low & address_mask != 0 || mask_high != 0 {
            let (base, size) = if kind == BarKind::Memory64 {
                let mask = (u64::from(mask_high) << 32) | u64::from(mask_low & address_mask);
                (
                    (u64::from(high) << 32) | u64::from(low & address_mask),
                    (!mask).checked_add(1).ok_or(Error::Range)?,
                )
            } else {
                (
                    u64::from(low & address_mask),
                    u64::from((!(mask_low & address_mask)).wrapping_add(1)),
                )
            };
            let bar = Bar {
                index: index as u8,
                kind,
                base,
                size,
                prefetch: kind != BarKind::Io && low & 8 != 0,
            };
            bar.end()?;
            for previous in result.iter().flatten() {
                if bar.conflicts(*previous)? {
                    return Err(Error::Conflict);
                }
            }
            result[index] = Some(bar);
        }
        index += if kind == BarKind::Memory64 { 2 } else { 1 };
    }
    Ok(result)
}

#[cfg(test)]
pub(crate) mod tests;
