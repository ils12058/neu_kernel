//! Validated ACPI input for PCI/VT-d. Physical reads are provided by the boot
//! backend; parsers never dereference firmware-provided pointers themselves.
use crate::pci::{Bdf, Ecam, Inventory, MAX_REGIONS};
pub const MAX_TABLE: usize = 4096;
pub const MAX_UNITS: usize = 4;
pub const MAX_SCOPES: usize = 32;
pub const MAX_RESERVED: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Access,
    Signature,
    Checksum,
    Length,
    Range,
    Reserved,
    Capacity,
    Conflict,
    Unsupported,
    Missing,
    MalformedPath,
}
pub type Result<T> = core::result::Result<T, Error>;
pub trait Reader {
    fn read(&self, physical: u64, output: &mut [u8]) -> Result<()>;
}

fn u16_at(b: &[u8], p: usize) -> u16 {
    u16::from_le_bytes(b[p..p + 2].try_into().unwrap())
}
fn u32_at(b: &[u8], p: usize) -> u32 {
    u32::from_le_bytes(b[p..p + 4].try_into().unwrap())
}
fn u64_at(b: &[u8], p: usize) -> u64 {
    u64::from_le_bytes(b[p..p + 8].try_into().unwrap())
}
fn checksum(b: &[u8]) -> Result<()> {
    if b.iter().fold(0u8, |sum, v| sum.wrapping_add(*v)) == 0 {
        Ok(())
    } else {
        Err(Error::Checksum)
    }
}
fn table<'a>(r: &impl Reader, address: u64, buffer: &'a mut [u8; MAX_TABLE]) -> Result<&'a [u8]> {
    if address == 0 {
        return Err(Error::Range);
    }
    r.read(address, &mut buffer[..36])?;
    let len = u32_at(buffer, 4) as usize;
    if !(36..=MAX_TABLE).contains(&len) || address.checked_add(len as u64).is_none() {
        return Err(Error::Length);
    }
    r.read(address, &mut buffer[..len])?;
    checksum(&buffer[..len])?;
    Ok(&buffer[..len])
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Unit {
    pub base: u64,
    pub segment: u16,
    pub include_all: bool,
    pub first_scope: u8,
    pub scope_count: u8,
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Scope {
    pub kind: u8,
    pub enumeration: u8,
    pub bus: u8,
    pub len: u8,
    pub path: [[u8; 2]; 8],
}
impl Scope {
    pub fn matches(self, inventory: &Inventory, segment: u16, target: Bdf) -> Result<bool> {
        if self.len == 0 || self.len > 8 {
            return Err(Error::Length);
        }
        if segment != target.segment || !matches!(self.kind, 1 | 2) {
            return Ok(false);
        }
        let mut bus = self.bus;
        for (index, pair) in self.path[..self.len as usize].iter().enumerate() {
            let bdf = Bdf {
                segment,
                bus,
                devfn: pair[0] * 8 + pair[1],
            };
            let f = inventory.get(bdf).map_err(|_| Error::Missing)?;
            if index + 1 == self.len as usize {
                return Ok(if self.kind == 1 {
                    bdf == target
                } else {
                    f.header == 1 && (f.secondary..=f.subordinate).contains(&target.bus)
                });
            }
            if f.header != 1 {
                return Err(Error::MalformedPath);
            }
            bus = f.secondary;
        }
        Err(Error::Length)
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReservedRange {
    pub base: u64,
    pub end: u64,
    pub segment: u16,
    pub first_scope: u8,
    pub scope_count: u8,
}
#[derive(Clone, Debug)]
pub struct Firmware {
    pub rsdp: u64,
    pub root: u64,
    pub mcfg: u64,
    pub dmar: u64,
    pub host_address_bits: u8,
    pub flags: u8,
    pub ecam: [Ecam; MAX_REGIONS],
    pub ecam_count: usize,
    pub units: [Unit; MAX_UNITS],
    pub unit_count: usize,
    pub scopes: [Scope; MAX_SCOPES],
    pub scope_count: usize,
    pub reserved: [ReservedRange; MAX_RESERVED],
    pub reserved_count: usize,
}
impl Default for Firmware {
    fn default() -> Self {
        Self::new()
    }
}
impl Firmware {
    pub const fn new() -> Self {
        Self {
            rsdp: 0,
            root: 0,
            mcfg: 0,
            dmar: 0,
            host_address_bits: 0,
            flags: 0,
            ecam: [Ecam {
                base: 0,
                segment: 0,
                first: 0,
                last: 0,
            }; MAX_REGIONS],
            ecam_count: 0,
            units: [Unit {
                base: 0,
                segment: 0,
                include_all: false,
                first_scope: 0,
                scope_count: 0,
            }; MAX_UNITS],
            unit_count: 0,
            scopes: [Scope {
                kind: 0,
                enumeration: 0,
                bus: 0,
                len: 0,
                path: [[0; 2]; 8],
            }; MAX_SCOPES],
            scope_count: 0,
            reserved: [ReservedRange {
                base: 0,
                end: 0,
                segment: 0,
                first_scope: 0,
                scope_count: 0,
            }; MAX_RESERVED],
            reserved_count: 0,
        }
    }
    pub fn discover(reader: &impl Reader, rsdp: u64) -> Result<Self> {
        let mut result = Self {
            rsdp,
            ..Self::new()
        };
        let mut buffer = [0u8; MAX_TABLE];
        reader.read(rsdp, &mut buffer[..20])?;
        if &buffer[..8] != b"RSD PTR " {
            return Err(Error::Signature);
        }
        checksum(&buffer[..20])?;
        let revision = buffer[15];
        let rsdt = u64::from(u32_at(&buffer, 16));
        let (root, width, signature) = if revision >= 2 {
            reader.read(rsdp, &mut buffer[..36])?;
            let len = u32_at(&buffer, 20) as usize;
            if !(36..=MAX_TABLE).contains(&len) {
                return Err(Error::Length);
            }
            reader.read(rsdp, &mut buffer[..len])?;
            checksum(&buffer[..len])?;
            if buffer[33..36] != [0; 3] {
                return Err(Error::Reserved);
            }
            let xsdt = u64_at(&buffer, 24);
            if xsdt != 0 {
                (xsdt, 8, *b"XSDT")
            } else {
                (rsdt, 4, *b"RSDT")
            }
        } else if revision == 0 {
            (rsdt, 4, *b"RSDT")
        } else {
            return Err(Error::Unsupported);
        };
        result.root = root;
        let root_bytes = table(reader, root, &mut buffer)?;
        if root_bytes[..4] != signature {
            return Err(Error::Signature);
        }
        if !(root_bytes.len() - 36).is_multiple_of(width) {
            return Err(Error::Length);
        }
        let mut addresses = [0u64; 128];
        let count = (root_bytes.len() - 36) / width;
        if count > addresses.len() {
            return Err(Error::Capacity);
        }
        for (i, chunk) in root_bytes[36..].chunks_exact(width).enumerate() {
            let address = if width == 8 {
                u64_at(chunk, 0)
            } else {
                u64::from(u32_at(chunk, 0))
            };
            if address == 0 || address == root || addresses[..i].contains(&address) {
                return Err(Error::Conflict);
            }
            addresses[i] = address;
        }
        for address in &addresses[..count] {
            reader.read(*address, &mut buffer[..4])?;
            if !matches!(&buffer[..4], b"MCFG" | b"DMAR") {
                continue;
            }
            let bytes = table(reader, *address, &mut buffer)?;
            if &bytes[..4] == b"MCFG" {
                if result.mcfg != 0 {
                    return Err(Error::Conflict);
                }
                result.mcfg = *address;
                result.parse_mcfg(bytes)?;
            } else {
                if result.dmar != 0 {
                    return Err(Error::Conflict);
                }
                result.dmar = *address;
                result.parse_dmar(bytes)?;
            }
        }
        if result.mcfg == 0 {
            return Err(Error::Missing);
        }
        Ok(result)
    }
    pub fn parse_mcfg(&mut self, bytes: &[u8]) -> Result<()> {
        if bytes.len() < 44 || !(bytes.len() - 44).is_multiple_of(16) {
            return Err(Error::Length);
        }
        if bytes.len() > MAX_TABLE || u32_at(bytes, 4) as usize != bytes.len() {
            return Err(Error::Length);
        }
        if &bytes[..4] != b"MCFG" {
            return Err(Error::Signature);
        }
        checksum(bytes)?;
        if bytes[36..44] != [0; 8] {
            return Err(Error::Reserved);
        }
        self.ecam_count = 0;
        for entry in bytes[44..].chunks_exact(16) {
            if self.ecam_count == MAX_REGIONS {
                return Err(Error::Capacity);
            }
            if entry[12..] != [0; 4] {
                return Err(Error::Reserved);
            }
            self.ecam[self.ecam_count] = Ecam {
                base: u64_at(entry, 0),
                segment: u16_at(entry, 8),
                first: entry[10],
                last: entry[11],
            };
            self.ecam_count += 1;
        }
        crate::pci::validate_regions(&self.ecam[..self.ecam_count]).map_err(|_| Error::Range)
    }
    fn parse_scopes(&mut self, bytes: &[u8]) -> Result<(u8, u8)> {
        let first = self.scope_count;
        let mut p = 0;
        while p < bytes.len() {
            if bytes.len() - p < 6 {
                return Err(Error::Length);
            }
            let len = bytes[p + 1] as usize;
            if len < 8 || len > bytes.len() - p || !(len - 6).is_multiple_of(2) {
                return Err(Error::Length);
            }
            let count = (len - 6) / 2;
            if count > 8 || self.scope_count == MAX_SCOPES {
                return Err(Error::Capacity);
            }
            if bytes[p + 2..p + 4] != [0; 2] || !matches!(bytes[p], 1..=4) {
                return Err(Error::Unsupported);
            }
            let mut s = Scope {
                kind: bytes[p],
                enumeration: bytes[p + 4],
                bus: bytes[p + 5],
                len: count as u8,
                ..Scope::default()
            };
            for (i, pair) in bytes[p + 6..p + len].chunks_exact(2).enumerate() {
                if pair[0] > 31 || pair[1] > 7 {
                    return Err(Error::Range);
                }
                s.path[i].copy_from_slice(pair);
            }
            self.scopes[self.scope_count] = s;
            self.scope_count += 1;
            p += len;
        }
        Ok((first as u8, (self.scope_count - first) as u8))
    }
    pub fn parse_dmar(&mut self, bytes: &[u8]) -> Result<()> {
        if bytes.len() < 48 {
            return Err(Error::Length);
        }
        if bytes.len() > MAX_TABLE || u32_at(bytes, 4) as usize != bytes.len() {
            return Err(Error::Length);
        }
        if &bytes[..4] != b"DMAR" {
            return Err(Error::Signature);
        }
        checksum(bytes)?;
        self.host_address_bits = bytes[36].checked_add(1).ok_or(Error::Range)?;
        if !(32..=64).contains(&self.host_address_bits) {
            return Err(Error::Range);
        }
        if bytes[37] & !7 != 0 || bytes[38..48] != [0; 10] {
            return Err(Error::Reserved);
        }
        self.flags = bytes[37];
        let mut p = 48;
        while p < bytes.len() {
            if bytes.len() - p < 4 {
                return Err(Error::Length);
            }
            let kind = u16_at(bytes, p);
            let len = u16_at(bytes, p + 2) as usize;
            if len < 4 || len > bytes.len() - p {
                return Err(Error::Length);
            }
            let e = &bytes[p..p + len];
            match kind {
                0 => {
                    if len < 16 {
                        return Err(Error::Length);
                    }
                    if self.unit_count == MAX_UNITS {
                        return Err(Error::Capacity);
                    }
                    if e[4] & !1 != 0 || e[5] != 0 {
                        return Err(Error::Reserved);
                    }
                    let mut unit = Unit {
                        base: u64_at(e, 8),
                        segment: u16_at(e, 6),
                        include_all: e[4] & 1 != 0,
                        ..Unit::default()
                    };
                    if unit.base == 0
                        || unit.base & 4095 != 0
                        || unit.base.checked_add(4096).is_none()
                        || (self.host_address_bits < 64 && unit.base >> self.host_address_bits != 0)
                    {
                        return Err(Error::Range);
                    }
                    if self.units[..self.unit_count].iter().any(|u| {
                        u.base == unit.base
                            || (u.segment == unit.segment && u.include_all && unit.include_all)
                    }) {
                        return Err(Error::Conflict);
                    }
                    (unit.first_scope, unit.scope_count) = self.parse_scopes(&e[16..])?;
                    if !unit.include_all && unit.scope_count == 0 {
                        return Err(Error::Missing);
                    }
                    self.units[self.unit_count] = unit;
                    self.unit_count += 1;
                }
                1 => {
                    if len < 24 {
                        return Err(Error::Length);
                    }
                    if self.reserved_count == MAX_RESERVED {
                        return Err(Error::Capacity);
                    }
                    if e[4..6] != [0; 2] {
                        return Err(Error::Reserved);
                    }
                    let base = u64_at(e, 8);
                    let end = u64_at(e, 16).checked_add(1).ok_or(Error::Range)?;
                    if base >= end || base & 4095 != 0 || end & 4095 != 0 {
                        return Err(Error::Range);
                    }
                    let (first_scope, scope_count) = self.parse_scopes(&e[24..])?;
                    if scope_count == 0 {
                        return Err(Error::Missing);
                    }
                    self.reserved[self.reserved_count] = ReservedRange {
                        base,
                        end,
                        segment: u16_at(e, 6),
                        first_scope,
                        scope_count,
                    };
                    self.reserved_count += 1;
                }
                // ATS remapping, proximity and namespace structures are not
                // needed by this non-ATS backend, but must remain well formed.
                2 => {
                    if len < 8 || e[4] & !1 != 0 || e[5] != 0 {
                        return Err(Error::Length);
                    }
                    self.parse_scopes(&e[8..])?;
                }
                3 => {
                    if len != 20 || e[4..8] != [0; 4] {
                        return Err(Error::Length);
                    }
                }
                4 => {
                    if len < 9 || e[4..7] != [0; 3] || e[len - 1] != 0 {
                        return Err(Error::Length);
                    }
                }
                _ => return Err(Error::Unsupported),
            }
            p += len;
        }
        if self.unit_count == 0 {
            return Err(Error::Missing);
        }
        Ok(())
    }
    pub fn unit_for(&self, inventory: &Inventory, bdf: Bdf) -> Result<usize> {
        let mut explicit = None;
        let mut fallback = None;
        for (index, unit) in self.units[..self.unit_count]
            .iter()
            .enumerate()
            .filter(|(_, u)| u.segment == bdf.segment)
        {
            if unit.include_all {
                fallback = Some(index);
            }
            for scope in &self.scopes
                [unit.first_scope as usize..(unit.first_scope + unit.scope_count) as usize]
            {
                if scope.matches(inventory, unit.segment, bdf)?
                    && explicit.replace(index).is_some_and(|old| old != index)
                {
                    return Err(Error::Conflict);
                }
            }
        }
        explicit.or(fallback).ok_or(Error::Missing)
    }
    pub fn requires_reserved_memory(&self, inventory: &Inventory, bdf: Bdf) -> Result<bool> {
        for region in &self.reserved[..self.reserved_count] {
            for scope in &self.scopes
                [region.first_scope as usize..(region.first_scope + region.scope_count) as usize]
            {
                if scope.matches(inventory, region.segment, bdf)? {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests;
