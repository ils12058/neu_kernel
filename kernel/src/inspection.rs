//! Read current bounded user mappings. No mutating table operation is used.
use crate::{
    node::{ActiveContext, NodeId},
    paging::{self, ADDRESS_FIELD, NX, PAGE, Registry, SpaceRef, TableMemory},
};
pub const PRESENT: u64 = 1;
pub const USER: u64 = 2;
pub const WRITE: u64 = 4;
pub const EXECUTE: u64 = 8;
/// Full trusted execution binding plus a real retained root, not a user selector.
pub fn scope_matches(
    expected: ActiveContext,
    actual: ActiveContext,
    owner: NodeId,
    reference: &SpaceRef,
    registry: &Registry,
    current_root: u64,
) -> bool {
    expected == actual
        && owner == actual.node
        && reference.id() == actual.context.space
        && reference.root() == actual.context.root
        && current_root == actual.context.root
        && registry.active_space() == Some(reference.id())
        && registry.pins(reference.id()).is_ok_and(|p| p != 0)
        && registry.validate_space_ref(reference).is_ok()
}
pub fn inspect(
    registry: &Registry,
    reference: &SpaceRef,
    memory: &impl TableMemory,
    bits: u8,
    va: u64,
) -> paging::Result<u64> {
    registry.validate_space_ref(reference)?;
    paging::user_range(va, 1)?;
    let path = paging::indices(va)?;
    let mut table = reference.root();
    let (mut user, mut write, mut execute) = (true, true, true);
    for (depth, index) in path.into_iter().enumerate() {
        let level = 4 - depth as u8;
        if !memory.owned(table, level) {
            return Err(paging::Error::NotOwned);
        }
        let raw = memory.read(table, index)?;
        if raw == 0 {
            return Ok(0);
        }
        let allowed = ADDRESS_FIELD | NX | 1 | 2 | 4 | 32 | if level == 1 { 64 } else { 0 };
        if raw & 1 == 0 || raw & !allowed != 0 {
            return Err(paging::Error::Flags);
        }
        let (address, size) = paging::decode(raw, level, bits)?.ok_or(paging::Error::Flags)?;
        user &= raw & 4 != 0;
        write &= raw & 2 != 0;
        execute &= raw & NX == 0;
        if size != 0 {
            if level != 1 || size != PAGE || write && execute {
                return Err(paging::Error::Flags);
            }
            return Ok(PRESENT
                | if user { USER } else { 0 }
                | if write { WRITE } else { 0 }
                | if execute { EXECUTE } else { 0 });
        }
        table = address;
    }
    Err(paging::Error::Corrupt)
}
