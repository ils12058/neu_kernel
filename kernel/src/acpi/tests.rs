use super::*;
use crate::pci::tests::{Mock, bdf, region};
use std::vec;
use std::vec::Vec;

fn seal(bytes: &mut [u8]) {
    bytes[9] = 0;
    bytes[9] = 0u8.wrapping_sub(bytes.iter().fold(0u8, |sum, b| sum.wrapping_add(*b)));
}
fn sdt(signature: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0; 36 + body.len()];
    let len = bytes.len() as u32;
    bytes[..4].copy_from_slice(signature);
    bytes[4..8].copy_from_slice(&len.to_le_bytes());
    bytes[8] = 1;
    bytes[36..].copy_from_slice(body);
    seal(&mut bytes);
    bytes
}
fn mcfg() -> Vec<u8> {
    let mut body = vec![0; 24];
    body[8..16].copy_from_slice(&0xe0000000u64.to_le_bytes());
    body[19] = 255;
    sdt(b"MCFG", &body)
}
fn drhd(base: u64, flags: u8, scopes: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0; 16 + scopes.len()];
    let len = bytes.len() as u16;
    bytes[2..4].copy_from_slice(&len.to_le_bytes());
    bytes[4] = flags;
    bytes[8..16].copy_from_slice(&base.to_le_bytes());
    bytes[16..].copy_from_slice(scopes);
    bytes
}
fn dmar(entries: &[u8]) -> Vec<u8> {
    let mut body = vec![0; 12];
    body[0] = 47;
    body.extend_from_slice(entries);
    sdt(b"DMAR", &body)
}
struct Memory(Vec<u8>);
impl Reader for Memory {
    fn read(&self, address: u64, output: &mut [u8]) -> Result<()> {
        let start = usize::try_from(address).map_err(|_| Error::Range)?;
        let end = start.checked_add(output.len()).ok_or(Error::Range)?;
        output.copy_from_slice(self.0.get(start..end).ok_or(Error::Access)?);
        Ok(())
    }
}
fn fixture() -> Memory {
    let mut memory = vec![0; 0x1000];
    let mc = mcfg();
    let dm = dmar(&drhd(0xfed90000, 1, &[]));
    let mut addresses = Vec::new();
    addresses.extend_from_slice(&0x200u64.to_le_bytes());
    addresses.extend_from_slice(&0x300u64.to_le_bytes());
    let xsdt = sdt(b"XSDT", &addresses);
    memory[0x100..0x100 + xsdt.len()].copy_from_slice(&xsdt);
    memory[0x200..0x200 + mc.len()].copy_from_slice(&mc);
    memory[0x300..0x300 + dm.len()].copy_from_slice(&dm);
    let r = &mut memory[0x40..0x64];
    r[..8].copy_from_slice(b"RSD PTR ");
    r[15] = 2;
    r[20..24].copy_from_slice(&36u32.to_le_bytes());
    r[24..32].copy_from_slice(&0x100u64.to_le_bytes());
    r[8] = 0u8.wrapping_sub(r[..20].iter().fold(0u8, |s, b| s.wrapping_add(*b)));
    r[32] = 0u8.wrapping_sub(r.iter().fold(0u8, |s, b| s.wrapping_add(*b)));
    Memory(memory)
}
#[test]
fn discovers_actual_table_addresses_and_default_deny_unit_description() {
    let f = Firmware::discover(&fixture(), 0x40).unwrap();
    assert_eq!(
        (f.root, f.mcfg, f.dmar, f.ecam_count, f.unit_count),
        (0x100, 0x200, 0x300, 1, 1)
    );
    assert_eq!(f.ecam[0].base, 0xe0000000);
    assert_eq!(f.units[0].base, 0xfed90000);
    assert_eq!(f.host_address_bits, 48);
}
#[test]
fn rejects_corruption_in_each_firmware_checksum() {
    for offset in [0x49, 0x61, 0x109, 0x209, 0x309] {
        let mut memory = fixture();
        memory.0[offset] ^= 1;
        assert!(
            Firmware::discover(&memory, 0x40).is_err(),
            "offset {offset:x}"
        );
    }
}
#[test]
fn rejects_oversized_tables_and_duplicate_root_entries() {
    let mut memory = fixture();
    memory.0[0x104..0x108].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(matches!(
        Firmware::discover(&memory, 0x40),
        Err(Error::Length)
    ));
    let mut memory = fixture();
    memory.0[0x12c..0x134].copy_from_slice(&0x200u64.to_le_bytes());
    seal(&mut memory.0[0x100..0x134]);
    assert!(matches!(
        Firmware::discover(&memory, 0x40),
        Err(Error::Conflict)
    ));
}
#[test]
fn refuses_missing_dmar_without_fabricating_an_isolation_unit() {
    let mut memory = fixture();
    memory.0[0x300..0x304].copy_from_slice(b"SSDT");
    let f = Firmware::discover(&memory, 0x40).unwrap();
    assert_eq!(f.unit_count, 0);
    assert_eq!(f.dmar, 0);
    assert_eq!(
        f.unit_for(&Inventory::new(), bdf(0, 8)),
        Err(Error::Missing)
    );
}
#[test]
fn rejects_reserved_mcfg_fields_aliases_and_misaligned_apertures() {
    for index in [36, 56, 44] {
        let mut bytes = mcfg();
        bytes[index] |= 1;
        seal(&mut bytes);
        assert!(Firmware::new().parse_mcfg(&bytes).is_err());
    }
    let bytes = mcfg();
    let mut body = bytes[36..].to_vec();
    body.extend_from_slice(&bytes[44..]);
    assert!(Firmware::new().parse_mcfg(&sdt(b"MCFG", &body)).is_err());
}
#[test]
fn rejects_duplicate_drhd_units_unknown_flags_and_truncated_scopes() {
    let unit = drhd(0xfed90000, 1, &[]);
    let mut entries = unit.clone();
    entries.extend_from_slice(&unit);
    assert_eq!(
        Firmware::new().parse_dmar(&dmar(&entries)),
        Err(Error::Conflict)
    );
    assert_eq!(
        Firmware::new().parse_dmar(&dmar(&drhd(0xfed90000, 2, &[]))),
        Err(Error::Reserved)
    );
    for scope in [
        &[1, 0, 0, 0, 0, 0][..],
        &[1, 8, 0, 0, 0, 0, 32, 0],
        &[1, 8, 0, 0, 0, 0, 0, 8],
    ] {
        assert!(
            Firmware::new()
                .parse_dmar(&dmar(&drhd(0xfed90000, 0, scope)))
                .is_err()
        );
    }
}
#[test]
fn explicit_requester_scope_wins_over_include_all() {
    let scope = [1, 8, 0, 0, 0, 0, 5, 0];
    let mut entries = drhd(0xfed91000, 0, &scope);
    entries.extend_from_slice(&drhd(0xfed90000, 1, &[]));
    let mut firmware = Firmware::new();
    firmware.parse_dmar(&dmar(&entries)).unwrap();
    let mut c = Mock::default();
    c.endpoint(bdf(0, 0x28));
    c.endpoint(bdf(0, 0x30));
    let inv = Inventory::enumerate(&mut c, &[region()]).unwrap();
    assert_eq!(firmware.unit_for(&inv, bdf(0, 0x28)), Ok(0));
    assert_eq!(firmware.unit_for(&inv, bdf(0, 0x30)), Ok(1));
}
#[test]
fn reserved_memory_scope_is_not_silently_ignored() {
    let mut entries = drhd(0xfed90000, 1, &[]);
    let mut rmrr = vec![0u8; 32];
    rmrr[0] = 1;
    rmrr[2] = 32;
    rmrr[8..16].copy_from_slice(&0x100000u64.to_le_bytes());
    rmrr[16..24].copy_from_slice(&0x100fffu64.to_le_bytes());
    rmrr[24..].copy_from_slice(&[1, 8, 0, 0, 0, 0, 5, 0]);
    entries.extend_from_slice(&rmrr);
    let mut f = Firmware::new();
    f.parse_dmar(&dmar(&entries)).unwrap();
    let mut c = Mock::default();
    c.endpoint(bdf(0, 0x28));
    c.endpoint(bdf(0, 0x30));
    let inv = Inventory::enumerate(&mut c, &[region()]).unwrap();
    assert_eq!(f.requires_reserved_memory(&inv, bdf(0, 0x28)), Ok(true));
    assert_eq!(f.requires_reserved_memory(&inv, bdf(0, 0x30)), Ok(false));
}
