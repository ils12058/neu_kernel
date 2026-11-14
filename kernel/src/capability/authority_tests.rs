use super::*;
use crate::{node::Nodes, paging, shootdown};
use std::{boxed::Box, collections::BTreeMap, vec::Vec};

fn delegated(access: Rights) -> Rights {
    access.union(Rights::DERIVE).union(Rights::REVOKE)
}
fn setup() -> (NodeId, NodeId, Table, Table, Objects) {
    let mut nodes = Nodes::new();
    let a = nodes.create().unwrap();
    let b = nodes.create().unwrap();
    (a, b, Table::new(a), Table::new(b), Objects::with_channels())
}
fn read(t: &Table, o: &mut Objects, owner: NodeId, h: Handle) -> Result<u64> {
    t.lookup(o, owner, h, ObjectType::Counter, Rights::READ)?
        .read()
}
fn purge(o: &mut Objects, a: &mut Table, b: &mut Table, ticket: &Revocation) {
    o.purge_revoked_operations().unwrap();
    a.purge_revoked(o).unwrap();
    b.purge_revoked(o).unwrap();
    o.finish_revoke(ticket).unwrap();
}

#[test]
fn authority_exhaustive_rights_subsets_never_amplify() {
    let (owner, _, mut table, _, mut objects) = setup();
    let mut registry = paging::Registry::new();
    let frame = registry.insert(0x100000).unwrap();
    registry.initialize(frame).unwrap();
    let memory = registry.create_memory_object(&[frame]).unwrap();
    let reference = registry.retain_memory_object(memory).unwrap();
    let creator = objects.memory_object(reference).map_err(|v| v.0).unwrap();
    let source_rights = delegated(Rights::READ.union(Rights::MAP));
    let source = table.grant(&mut objects, &creator, source_rights).unwrap();
    for bits in 1..=0x3ff {
        let requested = Rights::from_authority_bits(bits).unwrap();
        let before = objects.stats();
        let result = table.derive(&mut objects, owner, source, requested);
        let allowed = bits & !source_rights.bits() == 0 && bits & 0x0f != 0;
        assert_eq!(result.is_ok(), allowed, "rights={bits:#x}");
        if let Ok(child) = result {
            assert_eq!(table.describe(owner, child).unwrap().1.bits(), bits);
            assert!(
                table
                    .lookup(
                        &mut objects,
                        owner,
                        child,
                        ObjectType::MemoryObject,
                        Rights::WRITE
                    )
                    .is_err()
            );
            table.close(&mut objects, owner, child).unwrap();
        }
        assert_eq!(objects.stats(), before);
        assert_eq!(objects.live_grants(), 1);
    }
    assert_eq!(Rights::from_authority_bits(0x400), Err(Error::Rights));
    assert_eq!(Rights::from_bits(0x100), Err(Error::Rights));
}

#[test]
fn authority_nested_revoke_preserves_source_siblings_and_independent_grants() {
    let (a, b, mut ta, mut tb, mut o) = setup();
    let creator = o.counter(71).unwrap();
    let root = ta
        .grant(&mut o, &creator, delegated(Rights::READ_WRITE))
        .unwrap();
    let independent = tb
        .grant(&mut o, &creator, delegated(Rights::READ_WRITE))
        .unwrap();
    let branch = ta.derive(&mut o, a, root, delegated(Rights::READ)).unwrap();
    let leaf = ta.derive(&mut o, a, branch, Rights::READ).unwrap();
    let sibling = ta.derive(&mut o, a, root, Rights::READ).unwrap();
    o.release(creator).unwrap();
    let ticket = ta.begin_revoke(&mut o, a, branch).unwrap();
    assert_eq!(ticket.affected(), 1);
    assert_eq!(read(&ta, &mut o, a, leaf), Err(Error::Revoked));
    assert_eq!(o.finish_revoke(&ticket), Err(Error::InUse));
    for h in [root, branch, sibling] {
        assert_eq!(read(&ta, &mut o, a, h), Ok(71));
    }
    assert_eq!(read(&tb, &mut o, b, independent), Ok(71));
    purge(&mut o, &mut ta, &mut tb, &ticket);
    assert_eq!(read(&ta, &mut o, a, leaf), Err(Error::Stale));
    assert_eq!(o.finish_revoke(&ticket), Err(Error::Stale));
    let fresh = ta.derive(&mut o, a, branch, Rights::READ).unwrap();
    assert_ne!(fresh, leaf);
    let ticket = ta.begin_revoke(&mut o, a, root).unwrap();
    assert_eq!(ticket.affected(), 3);
    assert_eq!(ta.derive(&mut o, a, root, Rights::READ), Err(Error::InUse));
    purge(&mut o, &mut ta, &mut tb, &ticket);
    assert_eq!(ta.len(), 1);
    assert_eq!(read(&tb, &mut o, b, independent), Ok(71));
    ta.drain(&mut o, a).unwrap();
    tb.drain(&mut o, b).unwrap();
    assert_eq!(o.stats(), Stats::default());
    assert_eq!(o.live_grants(), 0);
}

#[test]
fn authority_close_is_not_revoke_and_closed_ancestors_cannot_be_reused() {
    let (a, _, mut t, _, mut o) = setup();
    let creator = o.counter(91).unwrap();
    let root = t
        .grant(&mut o, &creator, delegated(Rights::READ_WRITE))
        .unwrap();
    let child = t.derive(&mut o, a, root, delegated(Rights::READ)).unwrap();
    let leaf = t.derive(&mut o, a, child, Rights::READ).unwrap();
    let old = t.grant_id(&o, a, root).unwrap();
    t.close(&mut o, a, root).unwrap();
    t.close(&mut o, a, child).unwrap();
    assert_eq!(read(&t, &mut o, a, leaf), Ok(91));
    assert_eq!(o.live_grants(), 3);
    t.close(&mut o, a, leaf).unwrap();
    assert_eq!(o.live_grants(), 0);
    let fresh = t.grant(&mut o, &creator, delegated(Rights::READ)).unwrap();
    assert_ne!(old, t.grant_id(&o, a, fresh).unwrap());
    assert!(
        o.lineage
            .validate(old, creator.id(), delegated(Rights::READ_WRITE))
            .is_err()
    );
    t.drain(&mut o, a).unwrap();
    o.release(creator).unwrap();
    assert_eq!(o.stats(), Stats::default());
}

#[test]
fn authority_capacity_and_generation_exhaustion_do_not_publish_partial_grants() {
    let (a, _, mut t, _, mut o) = setup();
    let creator = o.counter(1).unwrap();
    let rights = delegated(Rights::READ);
    let mut parent = t.grant(&mut o, &creator, rights).unwrap();
    for _ in 1..GRANTS {
        let next = t.derive(&mut o, a, parent, rights).unwrap();
        t.close(&mut o, a, parent).unwrap();
        parent = next;
    }
    assert_eq!(o.live_grants(), GRANTS);
    let before = o.stats();
    assert_eq!(t.derive(&mut o, a, parent, rights), Err(Error::Capacity));
    assert_eq!(o.stats(), before);
    assert_eq!(t.len(), 1);
    t.close(&mut o, a, parent).unwrap();
    assert_eq!(o.live_grants(), 0);
    for slot in &mut t.slots {
        slot.generation = u32::MAX;
    }
    assert_eq!(t.grant(&mut o, &creator, rights), Err(Error::Capacity));
    assert_eq!(o.live_grants(), 0);
    o.release(creator).unwrap();
    assert_eq!(o.stats(), Stats::default());
}

fn channels() -> (
    NodeId,
    NodeId,
    Table,
    Table,
    Objects,
    Handle,
    Handle,
    Handle,
) {
    let (a, b, mut ta, mut tb, mut o) = setup();
    let creator = o.flag(true).unwrap();
    let root = ta
        .grant_movable(&mut o, &creator, delegated(Rights::READ))
        .unwrap();
    let independent = tb.grant_movable(&mut o, &creator, Rights::READ).unwrap();
    ta.bootstrap[3] = Some(root);
    tb.bootstrap[3] = Some(independent);
    o.install_channel(&mut ta, &mut tb).unwrap();
    ta.configure_moves().unwrap();
    tb.configure_moves().unwrap();
    let ea = ta.bootstrap[4].unwrap();
    let eb = tb.bootstrap[4].unwrap();
    o.release(creator).unwrap();
    (a, b, ta, tb, o, root, ea, eb)
}

#[test]
fn authority_queued_move_is_revoked_without_discarding_following_scalar() {
    let (a, b, mut ta, mut tb, mut o, root, ea, eb) = channels();
    let child = ta.derive(&mut o, a, root, Rights::READ).unwrap();
    let lineage = ta.grant_id(&o, a, child).unwrap();
    ta.send_cap(&mut o, a, ea, child).unwrap();
    ta.lookup(&mut o, a, ea, ObjectType::ChannelEndpoint, Rights::SEND)
        .unwrap()
        .channel_send(0x1234)
        .unwrap();
    assert_eq!(o.queued_references(), 1);
    let ticket = ta.begin_revoke(&mut o, a, root).unwrap();
    assert_eq!(tb.receive_cap(&mut o, b, eb), Err(Error::Revoked));
    purge(&mut o, &mut ta, &mut tb, &ticket);
    assert!(o.lineage.revoked(lineage));
    assert_eq!(o.queued_references(), 0);
    assert_eq!(
        tb.lookup(&mut o, b, eb, ObjectType::ChannelEndpoint, Rights::RECEIVE)
            .unwrap()
            .channel_receive(),
        Ok(0x1234)
    );
    ta.drain(&mut o, a).unwrap();
    tb.drain(&mut o, b).unwrap();
    assert_eq!(o.stats(), Stats::default());
    assert_eq!(o.live_grants(), 0);
}

#[test]
fn authority_delivered_moves_retain_lineage_and_reject_foreign_revocation() {
    let (a, b, mut ta, mut tb, mut o, root, ea, eb) = channels();
    let child = ta.derive(&mut o, a, root, Rights::READ).unwrap();
    let lineage = ta.grant_id(&o, a, child).unwrap();
    ta.send_cap(&mut o, a, ea, child).unwrap();
    let received = tb.receive_cap(&mut o, b, eb).unwrap();
    assert_eq!(tb.grant_id(&o, b, received), Ok(lineage));
    let before = o.stats();
    assert!(matches!(
        ta.begin_revoke(&mut o, b, root),
        Err(Error::Owner)
    ));
    assert!(matches!(
        tb.begin_revoke(&mut o, b, received),
        Err(Error::Rights)
    ));
    assert_eq!(o.stats(), before);
    let ticket = ta.begin_revoke(&mut o, a, root).unwrap();
    assert!(matches!(
        tb.lookup(&mut o, b, received, ObjectType::Flag, Rights::READ),
        Err(Error::Revoked)
    ));
    purge(&mut o, &mut ta, &mut tb, &ticket);
    assert_eq!(tb.len(), 5);
    ta.drain(&mut o, a).unwrap();
    tb.drain(&mut o, b).unwrap();
    assert_eq!(o.stats(), Stats::default());
}

struct Tables {
    next: u64,
    pages: BTreeMap<u64, (u8, Box<[u64; 512]>)>,
    invalidated: Vec<u64>,
    fail_write_after: Option<usize>,
}
impl Tables {
    fn new() -> Self {
        Self {
            next: 0x1000,
            pages: BTreeMap::new(),
            invalidated: Vec::new(),
            fail_write_after: None,
        }
    }
}
impl paging::TableMemory for Tables {
    fn allocate(&mut self, level: u8) -> paging::Result<u64> {
        let address = self.next;
        self.next += paging::PAGE;
        self.pages.insert(address, (level, Box::new([0; 512])));
        Ok(address)
    }
    fn release(&mut self, address: u64) -> paging::Result<()> {
        self.pages
            .remove(&address)
            .map(|_| ())
            .ok_or(paging::Error::NotOwned)
    }
    fn owned(&self, address: u64, level: u8) -> bool {
        self.pages.get(&address).is_some_and(|p| p.0 == level)
    }
    fn pinned(&self, _: u64) -> bool {
        false
    }
    fn read(&self, table: u64, index: usize) -> paging::Result<u64> {
        self.pages
            .get(&table)
            .and_then(|p| p.1.get(index))
            .copied()
            .ok_or(paging::Error::NotOwned)
    }
    fn write(&mut self, table: u64, index: usize, value: u64) -> paging::Result<()> {
        if let Some(remaining) = self.fail_write_after {
            self.fail_write_after = remaining.checked_sub(1);
            if remaining == 0 {
                return Err(paging::Error::Exhausted);
            }
        }
        *self
            .pages
            .get_mut(&table)
            .and_then(|p| p.1.get_mut(index))
            .ok_or(paging::Error::NotOwned)? = value;
        Ok(())
    }
    fn invalidate(&mut self, address: u64) {
        self.invalidated.push(address);
    }
    fn flush(&mut self) {
        self.invalidated.push(u64::MAX);
    }
}

#[test]
fn authority_mapping_survives_close_and_revoke_waits_for_every_cpu() {
    let (a, b, mut ta, mut tb, mut o) = setup();
    let mut registry = paging::Registry::new();
    let mut memory = Tables::new();
    let tree = paging::PageTree::new(&mut memory, 48).unwrap();
    let space = registry.create_space().unwrap();
    registry.bind_root(space, tree.root).unwrap();
    let backing = registry.insert(0x100000).unwrap();
    registry.initialize(backing).unwrap();
    let object = registry.create_memory_object(&[backing]).unwrap();
    let creator = o
        .memory_object(registry.retain_memory_object(object).unwrap())
        .map_err(|e| e.0)
        .unwrap();
    let root = ta
        .grant(&mut o, &creator, delegated(Rights::MEMORY_OBJECT))
        .unwrap();
    let child = ta
        .derive(&mut o, a, root, Rights::READ.union(Rights::MAP))
        .unwrap();
    let grant = ta.grant_id(&o, a, child).unwrap();
    let mut guard = ta
        .lookup(&mut o, a, child, ObjectType::MemoryObject, Rights::MAP)
        .unwrap();
    assert!(guard.memory_object(false).is_err());
    let request = MapRequest {
        space,
        root: tree.root,
        offset: 0,
        base: 0x410000,
        pages: 1,
        permissions: paging::Permissions::R,
    };
    guard
        .map_tracked(request, |permit| {
            let authorization = permit.authority(&registry)?;
            paging::map_memory_object(
                &tree,
                &mut memory,
                &mut registry,
                &authorization,
                request.offset,
                request.base,
                request.pages,
                request.permissions,
            )
        })
        .unwrap();
    drop(guard);
    assert_eq!(
        registry.mapping_for(space, request.base).unwrap().grant,
        grant
    );
    ta.close(&mut o, a, child).unwrap();
    o.release(creator).unwrap();
    registry.release_memory_object(object).unwrap();
    assert_eq!(o.tracked_mappings(), 1);
    assert_eq!(o.stats().pins, 1);
    registry.pin_space(space).unwrap();
    let ticket = ta.begin_revoke(&mut o, a, root).unwrap();
    ta.purge_revoked(&mut o).unwrap();
    tb.purge_revoked(&mut o).unwrap();
    assert_eq!(o.finish_revoke(&ticket), Err(Error::InUse));
    let binding = o
        .revocation_mappings(&ticket)
        .unwrap()
        .into_iter()
        .flatten()
        .next()
        .unwrap();
    let mut shoot = shootdown::Shootdown::new();
    let invalidation = shoot
        .begin(tree.root, request.base, 0b1111, 0b1111)
        .unwrap();
    o.validate_mapping_revoke(&ticket, binding, invalidation)
        .unwrap();
    registry
        .validate_mapping_revoke(space, grant, invalidation)
        .unwrap();
    tree.unmap_leaf_preserving_tables(&mut memory, request.base)
        .unwrap();
    registry
        .mark_mapping_revoke(space, grant, invalidation)
        .unwrap();
    o.mark_mapping_revoke(&ticket, binding, invalidation)
        .unwrap();
    assert_eq!(
        o.release_unmapped(&registry, space, request.base),
        Err(Error::InUse)
    );
    for cpu in [0, 2, 3] {
        shoot.acknowledge(cpu, invalidation).unwrap();
    }
    assert!(matches!(
        shoot.complete(invalidation.epoch),
        Err(shootdown::Error::Pending)
    ));
    assert_eq!(o.finish_revoke(&ticket), Err(Error::InUse));
    assert!(registry.collectible_memory_object(object).is_err());
    assert_eq!(registry.memory_object_counts(object).unwrap().1, 1);
    shoot.acknowledge(1, invalidation).unwrap();
    let receipt = shoot.complete(invalidation.epoch).unwrap();
    o.finish_mapping_revoke(&mut registry, &receipt).unwrap();
    assert_eq!(
        o.finish_mapping_revoke(&mut registry, &receipt),
        Err(Error::Stale)
    );
    assert_eq!(o.finish_revoke(&ticket), Ok(1));
    assert!(tree.translate(&memory, request.base).unwrap().is_none());
    assert_eq!(o.tracked_mappings(), 0);
    assert_eq!(o.stats().pins, 0);
    registry.unpin_space(space).unwrap();
    ta.drain(&mut o, a).unwrap();
    tb.drain(&mut o, b).unwrap();
    while let Some(reference) = o.drain_memory_object() {
        registry.release_memory_object_ref(reference).unwrap();
    }
    assert_eq!(
        registry.collect_memory_object(object).unwrap()[0],
        Some(backing)
    );
    assert_eq!(o.live_grants(), 0);
    assert_eq!(o.stats(), Stats::default());
}

#[test]
fn authority_real_host_cpus_serialize_derivation_moves_use_close_and_revocation() {
    use crate::sync::SpinLock;
    use std::{
        sync::{Arc, Barrier},
        thread,
    };
    struct Shared {
        a: NodeId,
        b: NodeId,
        ta: Table,
        tb: Table,
        objects: Objects,
        root: Handle,
        ea: Handle,
        eb: Handle,
        child: Option<Handle>,
        ticket: Option<Revocation>,
        baseline: Stats,
    }
    let (a, b, ta, tb, objects, root, ea, eb) = channels();
    let baseline = objects.stats();
    let shared = Arc::new(SpinLock::new(Shared {
        a,
        b,
        ta,
        tb,
        objects,
        root,
        ea,
        eb,
        child: None,
        ticket: None,
        baseline,
    }));
    let gate = Arc::new(Barrier::new(4));
    let workers: Vec<_> = (0..4)
        .map(|cpu| {
            let shared = Arc::clone(&shared);
            let gate = Arc::clone(&gate);
            thread::spawn(move || {
                for round in 0..128 {
                    let lock = || loop {
                        if let Some(g) = shared.lock_bounded(cpu + 1, 1000) {
                            break g;
                        }
                        thread::yield_now();
                    };
                    if cpu == 0 {
                        let mut g = lock();
                        let s = &mut *g;
                        s.child = Some(
                            s.ta.derive(&mut s.objects, s.a, s.root, delegated(Rights::READ))
                                .unwrap(),
                        );
                    }
                    gate.wait();
                    {
                        let mut g = lock();
                        let s = &mut *g;
                        let child = s.child.unwrap();
                        match cpu {
                            0 => {
                                let result = s.ta.derive(&mut s.objects, s.a, child, Rights::READ);
                                assert!(
                                    result.is_ok()
                                        || matches!(result, Err(Error::Stale | Error::Revoked))
                                );
                            }
                            1 => {
                                let result = s.ta.send_cap(&mut s.objects, s.a, s.ea, child);
                                assert!(result.is_ok() || result == Err(Error::Revoked));
                                if result.is_ok() {
                                    assert!(matches!(
                                        s.ta.lookup(
                                            &mut s.objects,
                                            s.a,
                                            child,
                                            ObjectType::Flag,
                                            Rights::READ
                                        ),
                                        Err(Error::Stale)
                                    ));
                                }
                            }
                            2 if round % 2 == 0 => {
                                let result = s.tb.receive_cap(&mut s.objects, s.b, s.eb);
                                assert!(
                                    result.is_ok()
                                        || matches!(result, Err(Error::Empty | Error::Revoked))
                                );
                            }
                            2 => {
                                let result =
                                    s.ta.lookup(
                                        &mut s.objects,
                                        s.a,
                                        child,
                                        ObjectType::Flag,
                                        Rights::READ,
                                    )
                                    .and_then(|g| g.read());
                                assert!(
                                    result == Ok(1)
                                        || matches!(result, Err(Error::Stale | Error::Revoked))
                                );
                            }
                            3 => {
                                s.ticket =
                                    Some(s.ta.begin_revoke(&mut s.objects, s.a, s.root).unwrap());
                            }
                            _ => unreachable!(),
                        }
                    }
                    gate.wait();
                    {
                        let mut g = lock();
                        let s = &mut *g;
                        match cpu {
                            0 => {
                                let result = s.ta.close(&mut s.objects, s.a, s.child.unwrap());
                                assert!(result.is_ok() || result == Err(Error::Stale));
                            }
                            1 => {
                                let independent = s.tb.bootstrap[3].unwrap();
                                assert_eq!(
                                    s.tb.lookup(
                                        &mut s.objects,
                                        s.b,
                                        independent,
                                        ObjectType::Flag,
                                        Rights::READ
                                    )
                                    .unwrap()
                                    .read(),
                                    Ok(1)
                                );
                            }
                            2 => {
                                let result = s.tb.receive_cap(&mut s.objects, s.b, s.eb);
                                assert!(matches!(result, Err(Error::Empty | Error::Revoked)));
                            }
                            3 => {
                                let ticket = s.ticket.take().unwrap();
                                purge(&mut s.objects, &mut s.ta, &mut s.tb, &ticket);
                            }
                            _ => unreachable!(),
                        }
                    }
                    gate.wait();
                    if cpu == 0 {
                        let mut g = lock();
                        let s = &mut *g;
                        assert_eq!(s.objects.stats(), s.baseline);
                        assert_eq!(s.objects.live_grants(), 1);
                        assert_eq!(s.objects.queued_references(), 0);
                        assert!(matches!(
                            s.ta.lookup(
                                &mut s.objects,
                                s.a,
                                s.child.unwrap(),
                                ObjectType::Flag,
                                Rights::READ
                            ),
                            Err(Error::Stale)
                        ));
                    }
                    gate.wait();
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    let mut g = shared.try_lock(5).unwrap();
    let s = &mut *g;
    s.ta.drain(&mut s.objects, s.a).unwrap();
    s.tb.drain(&mut s.objects, s.b).unwrap();
    assert_eq!(s.objects.stats(), Stats::default());
    assert_eq!(s.objects.live_grants(), 0);
}

fn authority_wait_fixture() -> (Nodes, Objects, [ActiveContext; 3], Handle, Handle, Handle) {
    use crate::node::ThreadContext;
    let mut n = Nodes::new();
    let mut r = paging::Registry::new();
    let mut active = Vec::new();
    for (index, root) in [(0, 0x1000), (1, 0x2000)] {
        let node = n.create().unwrap();
        n.configure(node).unwrap();
        let space = r.create_space().unwrap();
        r.bind_root(space, root).unwrap();
        n.bind_space(node, space).unwrap();
        let context = ThreadContext {
            space,
            root,
            entry: 0x400000,
            rsp: paging::thread_user_stack(index) + 4088,
        };
        let thread = n.initial_thread(node, context).unwrap();
        n.ready(node).unwrap();
        active.push(ActiveContext {
            node,
            thread,
            context,
        });
    }
    let context = ThreadContext {
        rsp: paging::thread_user_stack(2) + 4088,
        ..active[0].context
    };
    let thread = n.member_thread(active[0].node, context).unwrap();
    active.push(ActiveContext {
        node: active[0].node,
        thread,
        context,
    });
    let mut objects = Objects::with_channels();
    n.bootstrap_channel(&mut objects, active[0].node, active[1].node)
        .unwrap();
    let root = n.authority(active[0].node).unwrap().bootstrap[4].unwrap();
    let independent = n.authority(active[0].node).unwrap().bootstrap[5].unwrap();
    let peer = n.authority(active[1].node).unwrap().bootstrap[4].unwrap();
    n.authority_mut(active[0].node)
        .unwrap()
        .configure_delegation(&mut objects, root)
        .unwrap();
    for node in [active[0].node, active[1].node] {
        n.authority_mut(node).unwrap().seal_bootstrap();
    }
    (
        n,
        objects,
        active.try_into().unwrap(),
        root,
        independent,
        peer,
    )
}

#[test]
fn authority_revokes_closed_handle_waits_and_preserves_independent_endpoint_operations() {
    for sending in [false, true] {
        let (mut n, mut o, [a, b, sibling], root, independent, peer) = authority_wait_fixture();
        let rights = if sending {
            Rights::SEND
        } else {
            Rights::RECEIVE
        };
        let child = n
            .authority_mut(a.node)
            .unwrap()
            .derive(&mut o, a.node, root, rights)
            .unwrap();
        let start = |n: &Nodes, o: &mut Objects, active: ActiveContext, handle, value| {
            let mut guard = n
                .authority(active.node)
                .unwrap()
                .lookup(o, active.node, handle, ObjectType::ChannelEndpoint, rights)
                .unwrap();
            if sending {
                guard.channel_send_wait(active, value)
            } else {
                guard.channel_receive_wait(active)
            }
        };
        if sending {
            let mut g = n
                .authority(a.node)
                .unwrap()
                .lookup(
                    &mut o,
                    a.node,
                    root,
                    ObjectType::ChannelEndpoint,
                    Rights::SEND,
                )
                .unwrap();
            g.channel_send(1).unwrap();
            g.channel_send(2).unwrap();
        }
        assert_eq!(start(&n, &mut o, a, child, 99), Ok(WaitStart::Blocked));
        assert_eq!(
            start(&n, &mut o, sibling, independent, 3),
            Ok(WaitStart::Blocked)
        );
        n.authority_mut(a.node)
            .unwrap()
            .close(&mut o, a.node, child)
            .unwrap();
        assert_eq!(o.stats().pins, 2);
        let ticket = n
            .authority(a.node)
            .unwrap()
            .begin_revoke(&mut o, a.node, root)
            .unwrap();
        o.purge_revoked_operations().unwrap();
        n.purge_revoked_authority(&mut o).unwrap();
        assert_eq!(o.finish_revoke(&ticket), Ok(1));
        assert_eq!(o.stats().pins, 1);
        assert_eq!(o.take_ready_waits().iter().flatten().count(), 2);
        assert_eq!(o.settle_wait(a.thread), Settle::Failed(Error::Revoked));
        assert_eq!(o.settle_wait(sibling.thread), Settle::Retry);
        {
            let mut g = n
                .authority(b.node)
                .unwrap()
                .lookup(
                    &mut o,
                    b.node,
                    peer,
                    ObjectType::ChannelEndpoint,
                    Rights::SEND_RECEIVE,
                )
                .unwrap();
            if sending {
                assert_eq!(g.channel_receive(), Ok(1));
            } else {
                g.channel_send(42).unwrap();
            }
        }
        assert_eq!(o.take_ready_waits().iter().flatten().count(), 1);
        assert_eq!(
            o.settle_wait(sibling.thread),
            Settle::Committed(if sending { 0 } else { 42 })
        );
        if sending {
            let mut g = n
                .authority(b.node)
                .unwrap()
                .lookup(
                    &mut o,
                    b.node,
                    peer,
                    ObjectType::ChannelEndpoint,
                    Rights::RECEIVE,
                )
                .unwrap();
            assert_eq!(g.channel_receive(), Ok(2));
            assert_eq!(g.channel_receive(), Ok(3));
            assert_eq!(g.channel_receive(), Err(Error::Empty));
        }
        for node in [a.node, b.node] {
            n.authority_mut(node).unwrap().drain(&mut o, node).unwrap();
        }
        assert_eq!(o.stats(), Stats::default());
        assert_eq!(o.live_grants(), 0);
        assert_eq!(o.wait_stats(), crate::wait::Stats::default());
    }
}

#[test]
fn authority_ready_wait_rechecks_revocation_before_consuming_a_message() {
    let (mut n, mut o, [a, b, _], root, independent, peer) = authority_wait_fixture();
    let child = n
        .authority_mut(a.node)
        .unwrap()
        .derive(&mut o, a.node, root, Rights::RECEIVE)
        .unwrap();
    assert_eq!(
        n.authority(a.node)
            .unwrap()
            .lookup(
                &mut o,
                a.node,
                child,
                ObjectType::ChannelEndpoint,
                Rights::RECEIVE
            )
            .unwrap()
            .channel_receive_wait(a),
        Ok(WaitStart::Blocked)
    );
    n.authority(b.node)
        .unwrap()
        .lookup(
            &mut o,
            b.node,
            peer,
            ObjectType::ChannelEndpoint,
            Rights::SEND,
        )
        .unwrap()
        .channel_send(73)
        .unwrap();
    assert_eq!(o.take_ready_waits().iter().flatten().count(), 1);
    let ticket = n
        .authority(a.node)
        .unwrap()
        .begin_revoke(&mut o, a.node, root)
        .unwrap();
    assert_eq!(o.settle_wait(a.thread), Settle::Failed(Error::Revoked));
    assert_eq!(
        n.authority(a.node)
            .unwrap()
            .lookup(
                &mut o,
                a.node,
                independent,
                ObjectType::ChannelEndpoint,
                Rights::RECEIVE
            )
            .unwrap()
            .channel_receive(),
        Ok(73)
    );
    n.purge_revoked_authority(&mut o).unwrap();
    assert_eq!(o.finish_revoke(&ticket), Ok(1));
    for node in [a.node, b.node] {
        n.authority_mut(node).unwrap().drain(&mut o, node).unwrap();
    }
    assert_eq!(o.stats(), Stats::default());
    assert_eq!(o.live_grants(), 0);
}

#[test]
fn authority_discard_counter_exhaustion_rejects_before_revoking_or_detaching() {
    let (a, b, mut ta, mut tb, mut o, root, ea, eb) = channels();
    let child = ta.derive(&mut o, a, root, Rights::READ).unwrap();
    ta.send_cap(&mut o, a, ea, child).unwrap();
    o.channels.test_discarded([0, u64::MAX]);
    let before = o.stats();
    assert!(matches!(
        ta.begin_revoke(&mut o, a, root),
        Err(Error::Capacity)
    ));
    assert_eq!(o.stats(), before);
    assert_eq!(o.queued_references(), 1);
    let received = tb.receive_cap(&mut o, b, eb).unwrap();
    assert_eq!(
        tb.lookup(&mut o, b, received, ObjectType::Flag, Rights::READ)
            .unwrap()
            .read(),
        Ok(1)
    );
    o.channels.test_discarded([0, 0]);
    ta.drain(&mut o, a).unwrap();
    tb.drain(&mut o, b).unwrap();
    assert_eq!(o.stats(), Stats::default());
    assert_eq!(o.live_grants(), 0);
}

struct MappingFixture {
    node: NodeId,
    table: Table,
    objects: Objects,
    registry: paging::Registry,
    memory: Tables,
    tree: paging::PageTree,
    space: paging::SpaceId,
    backing: paging::MemoryObjectId,
    root: Handle,
    child: Handle,
}
impl MappingFixture {
    fn new() -> Self {
        let (node, _, mut table, _, mut objects) = setup();
        let mut registry = paging::Registry::new();
        let mut memory = Tables::new();
        let tree = paging::PageTree::new(&mut memory, 48).unwrap();
        let space = registry.create_space().unwrap();
        registry.bind_root(space, tree.root).unwrap();
        let frames = [
            registry.insert(0x100000).unwrap(),
            registry.insert(0x101000).unwrap(),
        ];
        for frame in frames {
            registry.initialize(frame).unwrap();
        }
        let backing = registry.create_memory_object(&frames).unwrap();
        let creator = objects
            .memory_object(registry.retain_memory_object(backing).unwrap())
            .map_err(|e| e.0)
            .unwrap();
        let root = table
            .grant(&mut objects, &creator, delegated(Rights::MEMORY_OBJECT))
            .unwrap();
        let child = table
            .derive(&mut objects, node, root, Rights::READ.union(Rights::MAP))
            .unwrap();
        objects.release(creator).unwrap();
        registry.release_memory_object(backing).unwrap();
        Self {
            node,
            table,
            objects,
            registry,
            memory,
            tree,
            space,
            backing,
            root,
            child,
        }
    }
    fn request(&self) -> MapRequest {
        MapRequest {
            space: self.space,
            root: self.tree.root,
            offset: 0,
            base: 0x5ff000,
            pages: 2,
            permissions: paging::Permissions::R,
        }
    }
    fn map(&mut self, request: MapRequest) -> Result<()> {
        let mut guard = self.table.lookup(
            &mut self.objects,
            self.node,
            self.child,
            ObjectType::MemoryObject,
            Rights::MAP,
        )?;
        guard.map_tracked(request, |permit| {
            let grant = permit.authority(&self.registry)?;
            paging::map_memory_object(
                &self.tree,
                &mut self.memory,
                &mut self.registry,
                &grant,
                request.offset,
                request.base,
                request.pages,
                request.permissions,
            )
        })
    }
    fn begin(&mut self) -> Revocation {
        self.registry.pin_space(self.space).unwrap();
        let ticket = self
            .table
            .begin_revoke(&mut self.objects, self.node, self.root)
            .unwrap();
        self.table.drain(&mut self.objects, self.node).unwrap(); // requester Node terminates
        ticket
    }
    fn remove(
        &mut self,
        ticket: &Revocation,
        binding: MappingBinding,
        shoot: &mut shootdown::Shootdown,
    ) -> shootdown::Request {
        let request = shoot
            .begin(binding.root(), binding.address(), 15, 15)
            .unwrap();
        self.objects
            .validate_mapping_revoke(ticket, binding, request)
            .unwrap();
        self.registry
            .validate_mapping_revoke(self.space, binding.grant(), request)
            .unwrap();
        self.tree
            .unmap_leaf_preserving_tables(&mut self.memory, binding.address())
            .unwrap();
        self.registry
            .mark_mapping_revoke(self.space, binding.grant(), request)
            .unwrap();
        self.objects
            .mark_mapping_revoke(ticket, binding, request)
            .unwrap();
        request
    }
}

#[test]
fn authority_mapping_preflight_and_partial_page_table_failure_publish_no_owners() {
    let mut f = MappingFixture::new();
    let request = f.request();
    let before = f.objects.stats();
    let tables = f.memory.pages.clone();
    for bad in [
        MapRequest {
            permissions: paging::Permissions::RW,
            ..request
        },
        MapRequest {
            root: request.root + 4096,
            ..request
        },
        MapRequest {
            offset: usize::MAX,
            ..request
        },
        MapRequest {
            base: request.base + 1,
            ..request
        },
        MapRequest {
            pages: 0,
            ..request
        },
    ] {
        assert!(f.map(bad).is_err());
        assert_eq!(f.objects.stats(), before);
        assert_eq!(f.objects.tracked_mappings(), 0);
        assert_eq!(f.registry.memory_object_counts(f.backing).unwrap().1, 0);
        assert_eq!(f.memory.pages, tables);
    }
    // The permit cannot authorize a wider range or even a second alias during
    // its valid borrow; all such failures precede any page-table publication.
    {
        let mut guard = f
            .table
            .lookup(
                &mut f.objects,
                f.node,
                f.child,
                ObjectType::MemoryObject,
                Rights::MAP,
            )
            .unwrap();
        assert_eq!(
            guard.map_tracked(request, |permit| {
                let grant = permit.authority(&f.registry)?;
                paging::map_memory_object(
                    &f.tree,
                    &mut f.memory,
                    &mut f.registry,
                    &grant,
                    request.offset,
                    request.base + 4096,
                    request.pages,
                    request.permissions,
                )
            }),
            Err(Error::Rights)
        );
    }
    f.memory.fail_write_after = Some(3);
    assert_eq!(f.map(request), Err(Error::Capacity));
    assert_eq!(f.memory.pages, tables);
    assert_eq!(f.objects.stats(), before);
    assert_eq!(f.objects.tracked_mappings(), 0);
    assert_eq!(f.registry.memory_object_counts(f.backing).unwrap().1, 0);
    f.map(request).unwrap();
    let owners = f.objects.stats();
    assert_eq!(f.map(request), Err(Error::InUse));
    assert_eq!(f.objects.stats(), owners);
    assert_eq!(f.objects.tracked_mappings(), 2);
}

#[test]
fn authority_pending_mapping_outlives_terminated_requester_and_rejects_wrong_receipts() {
    let mut f = MappingFixture::new();
    f.map(f.request()).unwrap();
    let ticket = f.begin();
    let bindings = f.objects.revocation_mappings(&ticket).unwrap();
    assert_eq!(f.objects.stats().references, 0);
    assert_eq!(f.objects.stats().pins, 2);
    assert_eq!(f.objects.live_grants(), 2);
    let mut shoot = shootdown::Shootdown::new();
    for (index, binding) in bindings.into_iter().flatten().enumerate() {
        let request = f.remove(&ticket, binding, &mut shoot);
        let mut foreign = shootdown::Shootdown::new();
        let wrong = foreign
            .begin(request.root + 4096, request.address, 15, 15)
            .unwrap();
        for cpu in 0..4 {
            foreign.acknowledge(cpu, wrong).unwrap();
        }
        let wrong = foreign.complete(wrong.epoch).unwrap();
        assert_eq!(
            f.objects.finish_mapping_revoke(&mut f.registry, &wrong),
            Err(Error::Stale)
        );
        for cpu in [0, 3, 1] {
            shoot.acknowledge(cpu, request).unwrap();
        }
        assert!(matches!(
            shoot.complete(request.epoch),
            Err(shootdown::Error::Pending)
        ));
        assert_eq!(f.objects.tracked_mappings(), 2 - index);
        assert_eq!(f.objects.finish_revoke(&ticket), Err(Error::InUse));
        shoot.acknowledge(2, request).unwrap();
        let receipt = shoot.complete(request.epoch).unwrap();
        f.objects
            .finish_mapping_revoke(&mut f.registry, &receipt)
            .unwrap();
        assert_eq!(
            f.objects.finish_mapping_revoke(&mut f.registry, &receipt),
            Err(Error::Stale)
        );
    }
    assert_eq!(f.objects.finish_revoke(&ticket), Ok(1));
    assert_eq!(f.objects.live_grants(), 0);
    assert_eq!(f.objects.tracked_mappings(), 0);
    f.registry.unpin_space(f.space).unwrap();
    f.registry
        .release_memory_object_ref(f.objects.drain_memory_object().unwrap())
        .unwrap();
    assert_eq!(
        f.registry
            .collect_memory_object(f.backing)
            .unwrap()
            .iter()
            .flatten()
            .count(),
        2
    );
    assert_eq!(f.objects.stats(), Stats::default());
}

#[test]
fn authority_shootdown_timeout_retains_mapping_backing_and_revocation_scope() {
    let mut f = MappingFixture::new();
    f.map(f.request()).unwrap();
    let ticket = f.begin();
    let binding = f
        .objects
        .revocation_mappings(&ticket)
        .unwrap()
        .into_iter()
        .flatten()
        .next()
        .unwrap();
    let mut shoot = shootdown::Shootdown::new();
    let request = f.remove(&ticket, binding, &mut shoot);
    shoot.acknowledge(0, request).unwrap();
    shoot.fail();
    for cpu in 1..4 {
        assert_eq!(
            shoot.acknowledge(cpu, request),
            Err(shootdown::Error::Failed)
        );
    }
    assert!(matches!(
        shoot.complete(request.epoch),
        Err(shootdown::Error::Failed)
    ));
    assert!(
        f.tree
            .translate(&f.memory, request.address)
            .unwrap()
            .is_none()
    );
    assert_eq!(f.objects.finish_revoke(&ticket), Err(Error::InUse));
    assert_eq!(f.objects.tracked_mappings(), 2);
    assert_eq!(f.objects.stats().pins, 2);
    assert_eq!(f.registry.memory_object_counts(f.backing).unwrap().1, 2);
    assert!(f.registry.collect_memory_object(f.backing).is_err());
    assert!(f.objects.drain_memory_object().is_none());
    assert_eq!(f.objects.live_grants(), 2);
}
