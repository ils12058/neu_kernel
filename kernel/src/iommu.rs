//! Intel VT-d legacy translation encodings. No pass-through/identity fallback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Unsupported,
    Address,
    Permissions,
    Domain,
    Registers,
}
pub type Result<T> = core::result::Result<T, Error>;
pub const PAGE: u64 = 4096;
pub const TRANSLATION_ENABLE: u32 = 1 << 31;
pub const SET_ROOT: u32 = 1 << 30;
pub const CONTEXT_INVALIDATE: u64 = (1 << 63) | (1 << 61);
pub const CONTEXT_COMPLETED: u64 = 1 << 59;
pub const IOTLB_INVALIDATE: u64 = (1 << 63) | (1 << 60);
pub const IOTLB_COMPLETED: u64 = 1 << 57;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub physical_bits: u8,
    pub levels: u8,
    pub domain_bits: u8,
    pub iotlb: u16,
    pub fault: u16,
    pub faults: u16,
    pub drain: u64,
    pub write_buffer_flush: bool,
}
impl Capabilities {
    pub fn parse(version: u32, cap: u64, ecap: u64, host_bits: u8) -> Result<Self> {
        if version & 0xfffffff0 != 0x10 || !(32..=52).contains(&host_bits) || cap & 7 == 7 {
            return Err(Error::Unsupported);
        }
        let levels = if cap & (2 << 8) != 0 {
            3
        } else if cap & (4 << 8) != 0 {
            4
        } else {
            return Err(Error::Unsupported);
        };
        let physical_bits = host_bits.min(((cap >> 16) & 63) as u8 + 1);
        if physical_bits < 32 {
            return Err(Error::Unsupported);
        }
        let iotlb = (((ecap >> 8) & 0x3ff) * 16 + 8) as u16;
        let fault = (((cap >> 24) & 0x3ff) * 16) as u16;
        let faults = ((cap >> 40) & 255) as u16 + 1;
        if iotlb < 0x80
            || usize::from(iotlb) + 8 > 4096
            || fault < 0x100
            || usize::from(fault) + usize::from(faults) * 16 > 4096
            || (iotlb >= fault && u32::from(iotlb) < u32::from(fault) + u32::from(faults) * 16)
        {
            return Err(Error::Registers);
        }
        let domain_bits = (4 + 2 * (cap & 7) as u8).min(16);
        Ok(Self {
            physical_bits,
            levels,
            domain_bits,
            iotlb,
            fault,
            faults,
            drain: if cap & (1 << 54) != 0 { 1 << 49 } else { 0 }
                | if cap & (1 << 55) != 0 { 1 << 48 } else { 0 },
            write_buffer_flush: cap & (1 << 4) != 0,
        })
    }
    pub fn context(self, root: u64, domain: u16) -> Result<[u64; 2]> {
        address(root, self.physical_bits)?;
        if domain == 0 || (self.domain_bits < 16 && u32::from(domain) >= 1 << self.domain_bits) {
            return Err(Error::Domain);
        }
        // TT=0, FPD=0: translated access and actual fault reporting.
        Ok([
            root | 1,
            u64::from(domain) << 8 | u64::from(self.levels - 2),
        ])
    }
}
pub fn address(physical: u64, bits: u8) -> Result<()> {
    if !(32..=52).contains(&bits) || physical == 0 || physical & 4095 != 0 || physical >> bits != 0
    {
        Err(Error::Address)
    } else {
        Ok(())
    }
}
pub fn root_entry(context: u64, bits: u8) -> Result<[u64; 2]> {
    address(context, bits)?;
    Ok([context | 1, 0])
}
pub fn entry(physical: u64, permissions: u8, bits: u8) -> Result<u64> {
    address(physical, bits)?;
    if permissions == 0 || permissions & !3 != 0 {
        return Err(Error::Permissions);
    }
    Ok(physical | u64::from(permissions))
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fault {
    pub requester: u16,
    pub address: u64,
    pub read: bool,
    pub reason: u8,
}
pub fn fault(low: u64, high: u64) -> Result<Fault> {
    // PP=0 denotes a legacy request. PASID-value bits 40..59 are then
    // undefined (QEMU writes its INVALID_PASID sentinel); never reject a real
    // legacy fault for their value. PP=1 and reserved fields remain unsupported.
    if high & (1 << 63) == 0 || high & 0x30000000ffff0000 != 0 {
        return Err(Error::Registers);
    }
    let reason = ((high >> 32) & 255) as u8;
    if reason == 0 {
        return Err(Error::Registers);
    }
    Ok(Fault {
        requester: high as u16,
        address: low & !4095,
        read: high & (1 << 62) != 0,
        reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn caps() -> Capabilities {
        Capabilities::parse(0x10, (0x22 << 24) | (47 << 16) | (2 << 8) | 6, 0xf00, 48).unwrap()
    }
    #[test]
    fn validates_hardware_capability_and_register_extents() {
        let c = caps();
        assert_eq!((c.levels, c.iotlb, c.fault, c.faults), (3, 0xf8, 0x220, 1));
        assert!(Capabilities::parse(0x20, 0, 0, 48).is_err());
        assert!(Capabilities::parse(0x10, 47 << 16, 0xf00, 48).is_err());
        assert!(
            Capabilities::parse(
                0x10,
                (0xff << 40) | (0xff << 24) | (47 << 16) | (2 << 8),
                0xf00,
                48
            )
            .is_err()
        );
        assert!(
            Capabilities::parse(0x10, (0x22 << 24) | (47 << 16) | (2 << 8) | 7, 0xf00, 48).is_err()
        );
        assert!(
            Capabilities::parse(0x10, (0x22 << 24) | (47 << 16) | (2 << 8), 0x2200, 48).is_err()
        );
        assert_eq!(
            Capabilities::parse(0x10, (0x22 << 24) | (47 << 16) | (4 << 8), 0xf00, 48)
                .unwrap()
                .levels,
            4
        );
    }
    #[test]
    fn context_is_translated_and_domains_never_truncate() {
        assert_eq!(caps().context(0x2000, 7), Ok([0x2001, 0x701]));
        assert!(caps().context(0x2000, 0).is_err());
        assert!(
            Capabilities {
                domain_bits: 4,
                ..caps()
            }
            .context(0x2000, 16)
            .is_err()
        );
        assert_eq!(root_entry(0x3000, 48), Ok([0x3001, 0]));
    }
    #[test]
    fn leaf_encoding_rejects_permissions_addresses_and_truncation() {
        for (p, r) in [(0, 3), (4097, 3), (1 << 48, 3), (4096, 0), (4096, 4)] {
            assert!(entry(p, r, 48).is_err());
        }
        assert_eq!(entry(0x1000, 1, 48), Ok(0x1001));
        assert_eq!(entry(0x1000, 2, 48), Ok(0x1002));
    }
    #[test]
    fn fault_record_preserves_requester_access_and_page() {
        assert_eq!(
            fault(0x101abc, (1 << 63) | (1 << 62) | (6 << 32) | 0x229),
            Ok(Fault {
                requester: 0x229,
                address: 0x101000,
                read: true,
                reason: 6
            })
        );
        assert_eq!(
            fault(0x9d3e000, (1 << 63) | (0xffff << 40) | (5 << 32) | 0x28),
            Ok(Fault {
                requester: 0x28,
                address: 0x9d3e000,
                read: false,
                reason: 5
            })
        );
        for bit in [16, 30, 31, 60, 61] {
            assert!(fault(0, (1 << 63) | (5 << 32) | (1 << bit)).is_err());
        }
        assert!(fault(0, 0).is_err());
    }
}
