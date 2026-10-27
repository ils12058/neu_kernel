use super::*;
use crate::{
    context::{SavedContext, StackBinding},
    node::{self, Nodes, ThreadContext, ThreadId},
    paging::Registry,
    scheduler::Queue,
};
fn owners() -> (Nodes, NodeId, NodeId) {
    let mut n = Nodes::new();
    let a = n.create().unwrap();
    let b = n.create().unwrap();
    (n, a, b)
}
fn counter(value: u64, rights: Rights) -> (Nodes, NodeId, Objects, Handle) {
    let (mut n, a, _) = owners();
    let mut o = Objects::new();
    let r = o.counter(value).unwrap();
    let h = n
        .authority_mut(a)
        .unwrap()
        .grant(&mut o, &r, rights)
        .unwrap();
    o.release(r).unwrap();
    (n, a, o, h)
}
fn read(n: &Nodes, o: &mut Objects, a: NodeId, h: Handle) -> Result<u64> {
    n.authority(a)
        .unwrap()
        .lookup(o, a, h, ObjectType::Counter, Rights::READ)?
        .read()
}
#[test]
fn each_created_node_has_a_distinct_exact_generation_table() {
    let (n, a, b) = owners();
    assert_ne!(n.authority(a).unwrap().id(), n.authority(b).unwrap().id());
    assert_eq!(n.authority(a).unwrap().id().owner(), a);
    assert!(n.authority(a).unwrap().is_empty());
}
#[test]
fn handles_encode_nonzero_bounded_slots_and_generations() {
    for slot in 0..HANDLES {
        for generation in [1, 2, u32::MAX] {
            let h = Handle::new(slot, generation);
            assert_eq!(h.parts(), Ok((slot, generation)));
            assert!(h.raw() > u32::MAX as u64);
        }
    }
}
#[test]
fn invalid_out_of_range_zero_generation_and_forged_integers_fail() {
    let (n, a, mut o, _) = counter(91, Rights::READ_WRITE);
    for raw in [
        0,
        1,
        8,
        9,
        1 << 32,
        (1 << 32) | 9,
        u64::MAX,
        0xdeadbeefcafebabe,
    ] {
        assert!(read(&n, &mut o, a, Handle::from_raw(raw)).is_err());
    }
    assert_eq!(
        o.stats(),
        Stats {
            live: 1,
            references: 1,
            pins: 0
        }
    );
}
#[test]
fn unallocated_slots_never_authorize_guessed_generations() {
    let (n, a, mut o, _) = counter(3, Rights::READ);
    assert_eq!(read(&n, &mut o, a, Handle::new(7, 1)), Err(Error::Handle));
}
#[test]
fn equal_numeric_handles_resolve_only_to_each_nodes_own_object() {
    let (mut n, a, b) = owners();
    let mut o = Objects::new();
    let ar = o.counter(11).unwrap();
    let br = o.counter(99).unwrap();
    let ah = n
        .authority_mut(a)
        .unwrap()
        .grant(&mut o, &ar, Rights::READ_WRITE)
        .unwrap();
    let bh = n
        .authority_mut(b)
        .unwrap()
        .grant(&mut o, &br, Rights::READ_WRITE)
        .unwrap();
    assert_eq!(ah, bh);
    assert_eq!(read(&n, &mut o, a, bh), Ok(11));
    assert_eq!(read(&n, &mut o, b, ah), Ok(99));
    assert_ne!(
        n.authority(a).unwrap().describe(a, ah).unwrap().0,
        n.authority(b).unwrap().describe(b, bh).unwrap().0
    );
}
#[test]
fn table_lookup_and_close_reject_another_node_identity() {
    let (mut n, a, b) = owners();
    let mut o = Objects::new();
    let r = o.counter(1).unwrap();
    let h = n
        .authority_mut(a)
        .unwrap()
        .grant(&mut o, &r, Rights::READ)
        .unwrap();
    assert!(matches!(
        n.authority(a)
            .unwrap()
            .lookup(&mut o, b, h, ObjectType::Counter, Rights::READ),
        Err(Error::Owner)
    ));
    assert_eq!(
        n.authority_mut(a).unwrap().close(&mut o, b, h),
        Err(Error::Owner)
    );
    assert_eq!(read(&n, &mut o, a, h), Ok(1));
}
#[test]
fn read_and_write_rights_are_independent() {
    for rights in [Rights::READ, Rights::WRITE] {
        let (n, a, mut o, h) = counter(5, rights);
        let t = n.authority(a).unwrap();
        if rights == Rights::READ {
            assert!(matches!(
                t.lookup(&mut o, a, h, ObjectType::Counter, Rights::WRITE),
                Err(Error::Rights)
            ));
            assert_eq!(read(&n, &mut o, a, h), Ok(5));
        } else {
            assert_eq!(read(&n, &mut o, a, h), Err(Error::Rights));
            let mut g = t
                .lookup(&mut o, a, h, ObjectType::Counter, Rights::WRITE)
                .unwrap();
            g.write(7).unwrap();
            assert_eq!(g.read(), Err(Error::Rights));
        }
    }
}
#[test]
fn combined_rights_are_required_for_exchange_and_guard_cannot_amplify() {
    let (n, a, mut o, h) = counter(8, Rights::READ_WRITE);
    let t = n.authority(a).unwrap();
    {
        let mut g = t
            .lookup(&mut o, a, h, ObjectType::Counter, Rights::READ)
            .unwrap();
        assert_eq!(g.write(12), Err(Error::Rights));
    }
    {
        let mut g = t
            .lookup(&mut o, a, h, ObjectType::Counter, Rights::READ_WRITE)
            .unwrap();
        assert_eq!(g.exchange(12), Ok(8));
    }
    assert_eq!(read(&n, &mut o, a, h), Ok(12));
    let (n, a, mut o, h) = counter(8, Rights::READ);
    assert!(matches!(
        n.authority(a)
            .unwrap()
            .lookup(&mut o, a, h, ObjectType::Counter, Rights::READ_WRITE),
        Err(Error::Rights)
    ));
}
#[test]
fn unknown_zero_and_object_incompatible_rights_do_not_publish_grants() {
    for bits in [0, 4, 8, 0x100, u64::MAX] {
        assert_eq!(Rights::from_bits(bits), Err(Error::Rights));
    }
    let (mut n, a, _) = owners();
    let mut o = Objects::new();
    let flag = o.flag(true).unwrap();
    let before = o.stats();
    assert_eq!(
        n.authority_mut(a)
            .unwrap()
            .grant(&mut o, &flag, Rights::WRITE),
        Err(Error::Rights)
    );
    assert_eq!(o.stats(), before);
    assert!(n.authority(a).unwrap().is_empty());
}
#[test]
fn wrong_object_type_fails_before_mutation_and_flag_has_real_storage() {
    let (mut n, a, _) = owners();
    let mut o = Objects::new();
    let r = o.flag(true).unwrap();
    let h = n
        .authority_mut(a)
        .unwrap()
        .grant(&mut o, &r, Rights::READ)
        .unwrap();
    assert!(matches!(
        n.authority(a)
            .unwrap()
            .lookup(&mut o, a, h, ObjectType::Counter, Rights::READ),
        Err(Error::Type)
    ));
    let g = n
        .authority(a)
        .unwrap()
        .lookup(&mut o, a, h, ObjectType::Flag, Rights::READ)
        .unwrap();
    assert_eq!(g.read(), Ok(1));
}
#[test]
fn object_identity_generation_and_type_are_checked_in_the_registry() {
    let mut o = Objects::new();
    let r = o.counter(8).unwrap();
    let id = r.id();
    let mut bad = id;
    bad.generation += 1;
    assert_eq!(o.info(bad), Err(Error::Object));
    bad = id;
    bad.kind = ObjectType::Flag;
    assert_eq!(o.info(bad), Err(Error::Type));
    o.release(r).unwrap();
    assert_eq!(o.info(id), Err(Error::Object));
}
#[test]
fn independent_grants_survive_close_and_last_reference_reclaims() {
    let (mut n, a, _) = owners();
    let mut o = Objects::new();
    let r = o.counter(31).unwrap();
    let id = r.id();
    let t = n.authority_mut(a).unwrap();
    let h = t.grant(&mut o, &r, Rights::READ_WRITE).unwrap();
    let other = t.grant(&mut o, &r, Rights::READ).unwrap();
    o.release(r).unwrap();
    n.authority_mut(a).unwrap().close(&mut o, a, h).unwrap();
    assert_eq!(read(&n, &mut o, a, h), Err(Error::Stale));
    assert_eq!(read(&n, &mut o, a, other), Ok(31));
    assert_eq!(o.info(id).unwrap().references, 1);
    n.authority_mut(a).unwrap().close(&mut o, a, other).unwrap();
    assert_eq!(o.stats(), Stats::default());
}
#[test]
fn double_close_cannot_release_another_reference() {
    let (mut n, a, mut o, h) = counter(8, Rights::READ);
    n.authority_mut(a).unwrap().close(&mut o, a, h).unwrap();
    assert_eq!(
        n.authority_mut(a).unwrap().close(&mut o, a, h),
        Err(Error::Stale)
    );
    assert_eq!(o.stats(), Stats::default());
}
#[test]
fn recycled_handle_and_object_slots_reject_old_generations() {
    let (mut n, a, mut o, old) = counter(8, Rights::READ);
    let old_id = n.authority(a).unwrap().describe(a, old).unwrap().0;
    n.authority_mut(a).unwrap().close(&mut o, a, old).unwrap();
    let r = o.counter(99).unwrap();
    let new_id = r.id();
    let new = n
        .authority_mut(a)
        .unwrap()
        .grant(&mut o, &r, Rights::READ)
        .unwrap();
    o.release(r).unwrap();
    assert_ne!(old, new);
    assert_eq!(old.parts().unwrap().0, new.parts().unwrap().0);
    assert_ne!(old_id, new_id);
    assert_eq!(old_id.slot, new_id.slot);
    assert_eq!(o.info(old_id), Err(Error::Object));
    assert_eq!(read(&n, &mut o, a, old), Err(Error::Stale));
    assert_eq!(read(&n, &mut o, a, new), Ok(99));
    assert_eq!(
        n.authority_mut(a).unwrap().close(&mut o, a, old),
        Err(Error::Stale)
    );
}
#[test]
fn authorized_operation_pin_survives_effective_last_handle_close() {
    let (mut n, a, mut o, h) = counter(44, Rights::READ);
    let t = n.authority_mut(a).unwrap();
    {
        let mut g = t
            .lookup(&mut o, a, h, ObjectType::Counter, Rights::READ)
            .unwrap();
        g.close_handle(t, h).unwrap();
        assert_eq!((g.info().references, g.info().pins), (0, 1));
        assert_eq!(g.read(), Ok(44));
        assert_eq!(t.describe(a, h), Err(Error::Stale));
    }
    assert_eq!(o.stats(), Stats::default());
}
#[test]
fn pinned_generation_cannot_be_recycled_until_guard_drops() {
    let (mut n, a, mut o, h) = counter(44, Rights::READ);
    let t = n.authority_mut(a).unwrap();
    let pinned_id;
    {
        let mut g = t
            .lookup(&mut o, a, h, ObjectType::Counter, Rights::READ)
            .unwrap();
        pinned_id = g.info().id;
        g.close_handle(t, h).unwrap();
        let r = g.objects.counter(55).unwrap();
        assert_ne!(r.id().slot, pinned_id.slot);
        g.objects.release(r).unwrap();
    }
    let r = o.counter(66).unwrap();
    assert_eq!(r.id().slot, pinned_id.slot);
    assert_ne!(r.id(), pinned_id);
}
#[test]
fn full_table_installation_is_atomic_for_references_and_generations() {
    let (mut n, a, _) = owners();
    let mut o = Objects::new();
    let r = o.counter(8).unwrap();
    let t = n.authority_mut(a).unwrap();
    for _ in 0..HANDLES {
        t.grant(&mut o, &r, Rights::READ).unwrap();
    }
    let before = o.stats();
    let generations = t.slots.each_ref().map(|s| s.generation);
    assert_eq!(t.grant(&mut o, &r, Rights::READ), Err(Error::Capacity));
    assert_eq!(t.slots.each_ref().map(|s| s.generation), generations);
    assert_eq!(o.stats(), before);
    assert_eq!(t.drain(&mut o, a), Ok(HANDLES));
    o.release(r).unwrap();
    assert_eq!(o.stats(), Stats::default());
}
#[test]
fn object_pool_exhaustion_does_not_publish_or_leak_an_object() {
    let mut o = Objects::new();
    let mut refs = std::vec::Vec::new();
    for i in 0..OBJECTS {
        refs.push(o.counter(i as u64).unwrap());
    }
    let before = o.stats();
    let generations = o.slots.each_ref().map(|s| s.generation);
    assert!(matches!(o.counter(999), Err(Error::Capacity)));
    assert_eq!(o.stats(), before);
    assert_eq!(o.slots.each_ref().map(|s| s.generation), generations);
    for r in refs {
        o.release(r).unwrap();
    }
    assert_eq!(o.stats(), Stats::default());
}
#[test]
fn reference_overflow_refuses_grant_without_consuming_a_handle_generation() {
    let (mut n, a, _) = owners();
    let mut o = Objects::new();
    let r = o.counter(1).unwrap();
    o.slots[r.id().slot].references = u32::MAX;
    let t = n.authority_mut(a).unwrap();
    assert_eq!(t.grant(&mut o, &r, Rights::READ), Err(Error::Capacity));
    assert!(t.is_empty());
    assert_eq!(t.slots[0].generation, 0);
}
#[test]
fn exhausted_handle_and_object_generations_never_wrap() {
    let (mut n, a, _) = owners();
    let mut o = Objects::new();
    o.slots[0].generation = u32::MAX;
    let r = o.counter(1).unwrap();
    assert_ne!(r.id().slot, 0);
    let t = n.authority_mut(a).unwrap();
    t.slots[0].generation = u32::MAX;
    let h = t.grant(&mut o, &r, Rights::READ).unwrap();
    assert_eq!(h.parts(), Ok((1, 1)));
    assert_eq!(t.slots[0].generation, u32::MAX);
}
fn runnable() -> (Nodes, Registry, Queue, [node::ThreadId; 3]) {
    let mut n = Nodes::new();
    let mut spaces = Registry::new();
    let mut q = Queue::new();
    let mut ids = std::vec::Vec::new();
    for (slot, root) in [(0, 0x1000), (1, 0x2000)] {
        let a = n.create().unwrap();
        n.configure(a).unwrap();
        let s = spaces.create_space().unwrap();
        spaces.bind_root(s, root).unwrap();
        n.bind_space(a, s).unwrap();
        let t = n
            .initial_thread(
                a,
                ThreadContext {
                    space: s,
                    root,
                    entry: 0x400000,
                    rsp: crate::paging::thread_user_stack(slot) + 4088,
                },
            )
            .unwrap();
        n.ready(a).unwrap();
        ids.push(t);
    }
    let mut c = n.thread(ids[0]).unwrap().context;
    c.rsp = crate::paging::thread_user_stack(2) + 4088;
    let a2 = n.member_thread(n.thread(ids[0]).unwrap().node, c).unwrap();
    ids.push(a2);
    for &id in &ids {
        let t = n.thread(id).unwrap();
        let active = ActiveContext {
            node: t.node,
            thread: id,
            context: t.context,
        };
        n.install_execution(
            id,
            SavedContext::initial_preemptible(active, StackBinding::new(active)),
        )
        .unwrap();
        q.enqueue(&mut n, id).unwrap();
    }
    (n, spaces, q, [ids[0], ids[2], ids[1]])
}
#[test]
fn table_lease_uses_actual_membership_and_blocks_suspension_until_returned() {
    let (mut n, _, mut q, _) = runnable();
    let d = q.next(&mut n).unwrap().unwrap();
    let lease = n.take_authority(d.active).unwrap();
    assert_eq!(lease.table().id().owner(), d.active.node);
    assert!(n.authority(d.active.node).is_err());
    assert!(n.take_authority(d.active).is_err());
    assert!(n.complete(d.active, node::ExitReason::Normal(1)).is_err());
    assert!(n.suspend_context(d.active, d.saved).is_err());
    n.restore_authority(lease).unwrap();
    q.finish(&mut n, d.active, node::ExitReason::Normal(1))
        .unwrap();
    assert_eq!(
        n.node(d.active.node).unwrap().state,
        node::NodeState::Active
    );
}
#[test]
fn sibling_threads_share_table_after_early_exit_but_terminal_node_requires_drain() {
    let (mut n, _, mut q, ids) = runnable();
    let a = n.thread(ids[0]).unwrap().node;
    let mut o = Objects::new();
    let r = o.counter(7).unwrap();
    let h = n
        .authority_mut(a)
        .unwrap()
        .grant(&mut o, &r, Rights::READ)
        .unwrap();
    o.release(r).unwrap();
    let id = n.authority(a).unwrap().id();
    let a1 = q.next(&mut n).unwrap().unwrap();
    q.finish(&mut n, a1.active, node::ExitReason::Normal(1))
        .unwrap();
    let b = q.next(&mut n).unwrap().unwrap();
    q.finish(&mut n, b.active, node::ExitReason::Normal(3))
        .unwrap();
    let a2 = q.next(&mut n).unwrap().unwrap();
    assert_eq!(a2.active.thread, ids[1]);
    let lease = n.take_authority(a2.active).unwrap();
    assert_eq!(lease.table().id(), id);
    assert_eq!(
        lease
            .table()
            .lookup(&mut o, a, h, ObjectType::Counter, Rights::READ)
            .unwrap()
            .read(),
        Ok(7)
    );
    n.restore_authority(lease).unwrap();
    q.finish(&mut n, a2.active, node::ExitReason::Normal(2))
        .unwrap();
    assert_eq!(n.authority_mut(a).unwrap().drain(&mut o, a), Ok(1));
    assert_eq!(o.stats(), Stats::default());
}
#[test]
fn fault_cancellation_prevents_future_leases_and_preserves_other_node_authority() {
    let (mut n, _, mut q, ids) = runnable();
    let a = n.thread(ids[0]).unwrap().node;
    let b = n.thread(ids[2]).unwrap().node;
    let mut o = Objects::new();
    let r = o.counter(77).unwrap();
    let h = n
        .authority_mut(b)
        .unwrap()
        .grant(&mut o, &r, Rights::READ)
        .unwrap();
    o.release(r).unwrap();
    let d = q.next(&mut n).unwrap().unwrap();
    q.finish(
        &mut n,
        d.active,
        node::ExitReason::UserFault(node::Fault {
            source: node::FaultSource::Cpu,
            vector: 6,
            error: 0,
            rip: 0x400000,
            rsp: 0x404ff8,
            cs: 0x3b,
            ss: 0x33,
            flags: 0x202,
            cr2: 0,
        }),
    )
    .unwrap();
    assert_eq!(n.authority_mut(a).unwrap().drain(&mut o, a), Ok(0));
    assert!(
        n.take_authority(ActiveContext {
            node: a,
            thread: ids[1],
            context: n.thread(ids[1]).unwrap().context
        })
        .is_err()
    );
    assert_eq!(read(&n, &mut o, b, h), Ok(77));
    assert!(q.contains(ids[2]));
}
#[test]
fn node_reclamation_refuses_live_authority_and_reuse_changes_table_identity() {
    let (mut n, a, _) = owners();
    let mut o = Objects::new();
    let r = o.flag(true).unwrap();
    n.authority_mut(a)
        .unwrap()
        .grant(&mut o, &r, Rights::READ)
        .unwrap();
    o.release(r).unwrap();
    let id = n.authority(a).unwrap().id();
    n.construction_failed(a).unwrap();
    assert_eq!(n.reclaim(a), Err(node::Error::InUse));
    n.authority_mut(a).unwrap().drain(&mut o, a).unwrap();
    n.reclaim(a).unwrap();
    assert!(n.authority(a).is_err());
    let new = n.create().unwrap();
    assert_ne!(new, a);
    assert_ne!(n.authority(new).unwrap().id(), id);
    assert_eq!(o.stats(), Stats::default());
}

#[test]
fn terminal_table_refuses_new_authority_but_existing_guard_can_finish() {
    let (mut n, a, _) = owners();
    let mut o = Objects::new();
    let r = o.counter(51).unwrap();
    let t = n.authority_mut(a).unwrap();
    let h = t.grant(&mut o, &r, Rights::READ).unwrap();
    {
        let mut g = t
            .lookup(&mut o, a, h, ObjectType::Counter, Rights::READ)
            .unwrap();
        t.terminate();
        assert_eq!(g.read(), Ok(51));
        assert!(matches!(
            t.lookup(g.objects, a, h, ObjectType::Counter, Rights::READ),
            Err(Error::Owner)
        ));
        g.close_handle(t, h).unwrap();
    }
    assert_eq!(t.grant(&mut o, &r, Rights::READ), Err(Error::Owner));
    o.release(r).unwrap();
    assert_eq!(o.stats(), Stats::default());
}

#[test]
fn lease_rejects_mixed_node_thread_space_root_and_stack_bindings() {
    let (mut n, _, mut q, ids) = runnable();
    let d = q.next(&mut n).unwrap().unwrap();
    let foreign = n.thread(ids[2]).unwrap();
    let mut bad = [d.active; 5];
    bad[0].node = foreign.node;
    bad[1].thread = foreign.id;
    bad[2].context.space = foreign.context.space;
    bad[3].context.root = foreign.context.root;
    bad[4].context.rsp = foreign.context.rsp;
    for active in bad {
        assert!(n.take_authority(active).is_err());
        assert_eq!(
            n.authority(d.active.node).unwrap().id().owner(),
            d.active.node
        );
    }
    let lease = n.take_authority(d.active).unwrap();
    n.restore_authority(lease).unwrap();
}

fn space_fixture() -> (Nodes, NodeId, Registry, crate::paging::SpaceId) {
    let mut n = Nodes::new();
    let a = n.create().unwrap();
    let mut r = Registry::new();
    let s = r.create_space().unwrap();
    r.bind_root(s, 0x1000).unwrap();
    (n, a, r, s)
}
#[test]
fn address_space_objects_own_real_retained_space_generations_and_defer_release() {
    let (mut n, a, mut r, s) = space_fixture();
    let mut o = Objects::new();
    let reference = r.retain_space(s).unwrap();
    let creator = o.address_space(a, reference).unwrap();
    let id = creator.id();
    let h = n
        .authority_mut(a)
        .unwrap()
        .grant(&mut o, &creator, Rights::INSPECT)
        .unwrap();
    o.release(creator).unwrap();
    {
        let g = n
            .authority(a)
            .unwrap()
            .lookup(&mut o, a, h, ObjectType::AddressSpace, Rights::INSPECT)
            .unwrap();
        let (owner, space) = g.address_space().unwrap();
        assert_eq!(owner, a);
        assert_eq!(space.id(), s);
        assert_eq!(space.root(), 0x1000);
        r.validate_space_ref(space).unwrap();
    }
    n.authority_mut(a).unwrap().close(&mut o, a, h).unwrap();
    assert_eq!(o.info(id), Err(Error::Object));
    assert_eq!(o.pending_spaces(), 1);
    assert_eq!(r.can_destroy_space(s), Err(crate::paging::Error::InUse));
    r.release_space_ref(o.reap_space().unwrap()).unwrap();
    assert_eq!(o.stats(), Stats::default());
    assert_eq!(r.can_destroy_space(s), Ok(()));
}
#[test]
fn inspect_right_is_distinct_and_inert_address_space_references_cannot_inspect() {
    let (mut n, a, mut r, s) = space_fixture();
    let mut o = Objects::new();
    let creator = o.address_space(a, r.retain_space(s).unwrap()).unwrap();
    let t = n.authority_mut(a).unwrap();
    for rights in [Rights::READ, Rights::WRITE, Rights::READ_WRITE] {
        assert_eq!(t.grant(&mut o, &creator, rights), Err(Error::Rights));
    }
    let inert = t.grant(&mut o, &creator, Rights::NONE).unwrap();
    assert!(matches!(
        t.lookup(&mut o, a, inert, ObjectType::AddressSpace, Rights::INSPECT),
        Err(Error::Rights)
    ));
    assert!(matches!(
        t.lookup(&mut o, a, inert, ObjectType::AddressSpace, Rights::NONE),
        Err(Error::Rights)
    ));
    let counter = o.counter(8).unwrap();
    assert_eq!(
        t.grant(&mut o, &counter, Rights::INSPECT),
        Err(Error::Rights)
    );
    let h = t.grant(&mut o, &counter, Rights::READ).unwrap();
    assert!(matches!(
        t.lookup(&mut o, a, h, ObjectType::AddressSpace, Rights::INSPECT),
        Err(Error::Type)
    ));
    t.drain(&mut o, a).unwrap();
    o.release(counter).unwrap();
    o.release(creator).unwrap();
    r.release_space_ref(o.reap_space().unwrap()).unwrap();
    assert_eq!(o.stats(), Stats::default());
}
#[test]
fn bootstrap_discovery_is_opaque_local_and_does_not_recreate_closed_authority() {
    let (mut n, a, mut r, s) = space_fixture();
    let mut o = Objects::new();
    let creator = o.address_space(a, r.retain_space(s).unwrap()).unwrap();
    let t = n.authority_mut(a).unwrap();
    let h = t
        .install_bootstrap(&mut o, &creator, Rights::INSPECT, 0)
        .unwrap();
    let independent = t
        .install_bootstrap(&mut o, &creator, Rights::INSPECT, 1)
        .unwrap();
    t.seal_bootstrap();
    assert_eq!(t.bootstrap_get(&o, a, 0), Ok(h));
    assert_eq!(t.bootstrap_get(&o, a, 1), Ok(independent));
    t.close(&mut o, a, h).unwrap();
    assert_eq!(t.bootstrap_get(&o, a, 0), Err(Error::Stale));
    let replacement = t.grant(&mut o, &creator, Rights::INSPECT).unwrap();
    assert_ne!(replacement, h);
    assert_eq!(replacement.parts().unwrap().0, h.parts().unwrap().0);
    assert_eq!(t.bootstrap_get(&o, a, 0), Err(Error::Stale));
    assert_eq!(t.bootstrap_get(&o, a, 1), Ok(independent));
    assert_eq!(t.bootstrap_get(&o, a, 4), Err(Error::Handle));
    assert_eq!(
        t.install_bootstrap(&mut o, &creator, Rights::INSPECT, 2),
        Err(Error::InUse)
    );
    t.drain(&mut o, a).unwrap();
    o.release(creator).unwrap();
    r.release_space_ref(o.reap_space().unwrap()).unwrap();
}
#[test]
fn bootstrap_installation_failure_does_not_publish_a_manifest_entry_or_reference() {
    let (mut n, a, mut r, s) = space_fixture();
    let mut o = Objects::new();
    let creator = o.address_space(a, r.retain_space(s).unwrap()).unwrap();
    let t = n.authority_mut(a).unwrap();
    let before = o.stats();
    assert_eq!(
        t.install_bootstrap(&mut o, &creator, Rights::READ, 0),
        Err(Error::Rights)
    );
    assert_eq!(o.stats(), before);
    assert_eq!(t.slots[0].generation, 0);
    assert!(t.bootstrap[0].is_none());
    for _ in 0..HANDLES {
        t.grant(&mut o, &creator, Rights::INSPECT).unwrap();
    }
    let before = o.stats();
    assert_eq!(
        t.install_bootstrap(&mut o, &creator, Rights::INSPECT, 0),
        Err(Error::Capacity)
    );
    assert_eq!(o.stats(), before);
    assert!(t.bootstrap[0].is_none());
    t.drain(&mut o, a).unwrap();
    o.release(creator).unwrap();
    r.release_space_ref(o.reap_space().unwrap()).unwrap();
}
#[test]
fn failed_address_space_object_creation_returns_the_root_reference_for_rollback() {
    let (_, a, mut r, s) = space_fixture();
    let mut o = Objects::new();
    let mut refs = std::vec::Vec::new();
    for _ in 0..OBJECTS {
        refs.push(o.counter(7).unwrap());
    }
    let before = o.stats();
    let (error, reference) = match o.address_space(a, r.retain_space(s).unwrap()) {
        Err(error) => error,
        Ok(_) => panic!("pool unexpectedly had capacity"),
    };
    assert_eq!(error, Error::Capacity);
    assert_eq!(o.stats(), before);
    r.release_space_ref(reference).unwrap();
    assert_eq!(r.object_references(s), Ok(0));
    for reference in refs {
        o.release(reference).unwrap();
    }
}
#[test]
fn closing_last_address_space_handle_while_pinned_keeps_the_root_until_common_reap() {
    let (mut n, a, mut r, s) = space_fixture();
    let mut o = Objects::new();
    let creator = o.address_space(a, r.retain_space(s).unwrap()).unwrap();
    let t = n.authority_mut(a).unwrap();
    let h = t.grant(&mut o, &creator, Rights::INSPECT).unwrap();
    o.release(creator).unwrap();
    {
        let mut g = t
            .lookup(&mut o, a, h, ObjectType::AddressSpace, Rights::INSPECT)
            .unwrap();
        g.close_handle(t, h).unwrap();
        assert_eq!((g.info().references, g.info().pins), (0, 1));
        let (_, reference) = g.address_space().unwrap();
        r.validate_space_ref(reference).unwrap();
        assert!(g.objects.reap_space().is_none());
        assert_eq!(r.can_destroy_space(s), Err(crate::paging::Error::InUse));
    }
    assert_eq!(o.pending_spaces(), 1);
    r.release_space_ref(o.reap_space().unwrap()).unwrap();
    assert_eq!(o.stats(), Stats::default());
    assert_eq!(r.can_destroy_space(s), Ok(()));
}
#[test]
fn expired_or_reused_space_generation_is_rejected_despite_equal_root_numbers() {
    let (_, a, mut r, s) = space_fixture();
    let mut o = Objects::new();
    let creator = o.address_space(a, r.retain_space(s).unwrap()).unwrap();
    let mut t = Table::new(a);
    let h = t.grant(&mut o, &creator, Rights::INSPECT).unwrap();
    o.release(creator).unwrap();
    let mut reused = Registry::new();
    let old = reused.create_space().unwrap();
    reused.bind_root(old, 0x1000).unwrap();
    reused.clear_root(old, 0x1000).unwrap();
    reused.retire_space(old).unwrap();
    let fresh = reused.create_space().unwrap();
    reused.bind_root(fresh, 0x1000).unwrap();
    assert_ne!(old, fresh);
    {
        let g = t
            .lookup(&mut o, a, h, ObjectType::AddressSpace, Rights::INSPECT)
            .unwrap();
        let (_, reference) = g.address_space().unwrap();
        assert_eq!(reference.id(), old);
        assert_eq!(
            reused.validate_space_ref(reference),
            Err(crate::paging::Error::NotOwned)
        );
    }
    t.close(&mut o, a, h).unwrap();
    r.release_space_ref(o.reap_space().unwrap()).unwrap();
}
#[test]
fn immutable_inspection_binding_rejects_wrong_node_thread_space_root_and_missing_pins() {
    let (mut n, mut r, mut q, ids) = runnable();
    let d = q.next(&mut n).unwrap().unwrap();
    let other = n.thread(ids[2]).unwrap();
    let reference = r.retain_space(d.active.context.space).unwrap();
    r.activate_space(reference.id()).unwrap();
    assert!(!crate::inspection::scope_matches(
        d.active,
        d.active,
        d.active.node,
        &reference,
        &r,
        reference.root()
    ));
    r.deactivate_space(reference.id()).unwrap();
    r.pin_space(reference.id()).unwrap();
    r.activate_space(reference.id()).unwrap();
    assert!(crate::inspection::scope_matches(
        d.active,
        d.active,
        d.active.node,
        &reference,
        &r,
        reference.root()
    ));
    let mut bad = [d.active; 4];
    bad[0].node = other.node;
    bad[1].thread = other.id;
    bad[2].context.space = other.context.space;
    bad[3].context.root = other.context.root;
    for actual in bad {
        assert!(!crate::inspection::scope_matches(
            d.active,
            actual,
            d.active.node,
            &reference,
            &r,
            reference.root()
        ));
    }
    assert!(!crate::inspection::scope_matches(
        d.active,
        d.active,
        other.node,
        &reference,
        &r,
        reference.root()
    ));
    r.deactivate_space(reference.id()).unwrap();
    r.unpin_space(reference.id()).unwrap();
    r.release_space_ref(reference).unwrap();
}

fn channels() -> (Nodes, Objects, NodeId, NodeId) {
    let (mut n, a, b) = owners();
    let mut o = Objects::with_channels();
    n.bootstrap_channel(&mut o, a, b).unwrap();
    for id in [a, b] {
        n.authority_mut(id).unwrap().seal_bootstrap();
    }
    (n, o, a, b)
}
fn node_with_thread(
    n: &mut Nodes,
    spaces: &mut Registry,
    root: u64,
    rsp: u64,
) -> (NodeId, ThreadId, ActiveContext) {
    let space = spaces.create_space().unwrap();
    spaces.bind_root(space, root).unwrap();
    let node = n.create().unwrap();
    n.configure(node).unwrap();
    n.bind_space(node, space).unwrap();
    let context = ThreadContext {
        space,
        root,
        entry: 0x400000,
        rsp,
    };
    let thread = n.initial_thread(node, context).unwrap();
    n.ready(node).unwrap();
    (
        node,
        thread,
        ActiveContext {
            node,
            thread,
            context,
        },
    )
}
/// Three real Thread identities in the supported two-Node topology: A has two
/// members sharing authority, B has one peer Thread.
fn waiters() -> (Nodes, Objects, ActiveContext, ActiveContext, ActiveContext) {
    let mut n = Nodes::new();
    let mut spaces = Registry::new();
    let (a, _, a1) = node_with_thread(&mut n, &mut spaces, 0x1000, 0x404ff8);
    let a2 = n
        .member_thread(
            a,
            ThreadContext {
                space: a1.context.space,
                root: 0x1000,
                entry: 0x400000,
                rsp: 0x402ff8,
            },
        )
        .unwrap();
    let a2 = ActiveContext {
        node: a,
        thread: a2,
        context: n.thread(a2).unwrap().context,
    };
    let (b, _, b1) = node_with_thread(&mut n, &mut spaces, 0x2000, 0x404ff8);
    let mut o = Objects::with_channels();
    n.bootstrap_channel(&mut o, a, b).unwrap();
    for id in [a, b] {
        n.authority_mut(id).unwrap().seal_bootstrap();
    }
    (n, o, a1, a2, b1)
}
fn receive_start(
    n: &Nodes,
    o: &mut Objects,
    active: ActiveContext,
    handle: Handle,
) -> Result<WaitStart> {
    n.authority(active.node)
        .unwrap()
        .lookup(
            o,
            active.node,
            handle,
            ObjectType::ChannelEndpoint,
            Rights::RECEIVE,
        )?
        .channel_receive_wait(active)
}
fn send_start(
    n: &Nodes,
    o: &mut Objects,
    active: ActiveContext,
    handle: Handle,
    payload: u64,
) -> Result<WaitStart> {
    n.authority(active.node)
        .unwrap()
        .lookup(
            o,
            active.node,
            handle,
            ObjectType::ChannelEndpoint,
            Rights::SEND,
        )?
        .channel_send_wait(active, payload)
}
fn queued(o: &mut Objects) -> ([Option<crate::wait::Waiter>; 3], usize) {
    let ready = o.take_ready_waits();
    let count = ready.iter().flatten().count();
    (ready, count)
}
fn close_all(n: &mut Nodes, o: &mut Objects, node: NodeId) {
    for index in 4..8 {
        let h = n
            .authority(node)
            .unwrap()
            .bootstrap_get(o, node, index)
            .unwrap();
        n.authority_mut(node).unwrap().close(o, node, h).unwrap();
    }
}
fn endpoint(n: &Nodes, o: &Objects, node: NodeId, index: u64) -> Handle {
    n.authority(node)
        .unwrap()
        .bootstrap_get(o, node, index)
        .unwrap()
}
fn send(n: &Nodes, o: &mut Objects, node: NodeId, h: Handle, v: u64) -> Result<()> {
    n.authority(node)
        .unwrap()
        .lookup(o, node, h, ObjectType::ChannelEndpoint, Rights::SEND)?
        .channel_send(v)
}
fn receive(n: &Nodes, o: &mut Objects, node: NodeId, h: Handle) -> Result<u64> {
    n.authority(node)
        .unwrap()
        .lookup(o, node, h, ObjectType::ChannelEndpoint, Rights::RECEIVE)?
        .channel_receive()
}
#[test]
fn typed_endpoint_caps_authorize_real_bidirectional_queue_operations() {
    let (n, mut o, a, b) = channels();
    let ah = endpoint(&n, &o, a, 4);
    let bh = endpoint(&n, &o, b, 4);
    assert_eq!(ah, bh);
    assert_ne!(
        n.authority(a).unwrap().describe(a, ah).unwrap().0,
        n.authority(b).unwrap().describe(b, bh).unwrap().0
    );
    send(&n, &mut o, a, ah, 0).unwrap();
    send(&n, &mut o, b, bh, u64::MAX).unwrap();
    assert_eq!(receive(&n, &mut o, b, bh), Ok(0));
    assert_eq!(receive(&n, &mut o, a, ah), Ok(u64::MAX));
}
#[test]
fn send_and_receive_rights_are_independent_and_denials_preserve_messages() {
    let (n, mut o, a, b) = channels();
    let send_only = endpoint(&n, &o, a, 6);
    let recv_only = endpoint(&n, &o, a, 7);
    let bh = endpoint(&n, &o, b, 4);
    send(&n, &mut o, b, bh, 7).unwrap();
    let before = o.channel_state().snapshot();
    assert_eq!(receive(&n, &mut o, a, send_only), Err(Error::Rights));
    assert_eq!(send(&n, &mut o, a, recv_only, 9), Err(Error::Rights));
    assert_eq!(o.channel_state().snapshot(), before);
    assert_eq!(receive(&n, &mut o, a, recv_only), Ok(7));
}
#[test]
fn wrong_types_and_malformed_rights_do_not_touch_channel_storage() {
    let (mut n, mut o, a, _) = channels();
    let c = o.counter(8).unwrap();
    let h = n
        .authority_mut(a)
        .unwrap()
        .grant(&mut o, &c, Rights::READ_WRITE)
        .unwrap();
    let before = o.channel_state().snapshot();
    assert_eq!(send(&n, &mut o, a, h, 9), Err(Error::Type));
    assert_eq!(receive(&n, &mut o, a, h), Err(Error::Type));
    assert_eq!(o.channel_state().snapshot(), before);
    for bits in [4, 8, 128, u64::MAX] {
        assert_eq!(Rights::from_bits(bits), Err(Error::Rights));
    }
    for rights in [Rights::SEND, Rights::RECEIVE, Rights::SEND_RECEIVE] {
        assert_eq!(
            n.authority_mut(a).unwrap().grant(&mut o, &c, rights),
            Err(Error::Rights)
        );
    }
}
#[test]
fn closing_one_endpoint_handle_preserves_independent_authority_and_stale_close_is_inert() {
    let (mut n, mut o, a, b) = channels();
    let h = endpoint(&n, &o, a, 4);
    let other = endpoint(&n, &o, a, 5);
    let bh = endpoint(&n, &o, b, 4);
    n.authority_mut(a).unwrap().close(&mut o, a, h).unwrap();
    send(&n, &mut o, a, other, 19).unwrap();
    let before = o.channel_state().snapshot();
    assert_eq!(send(&n, &mut o, a, h, 20), Err(Error::Stale));
    assert_eq!(
        n.authority_mut(a).unwrap().close(&mut o, a, h),
        Err(Error::Stale)
    );
    assert_eq!(o.channel_state().snapshot(), before);
    assert_eq!(receive(&n, &mut o, b, bh), Ok(19));
    assert_eq!(
        n.authority(a).unwrap().bootstrap_get(&o, a, 4),
        Err(Error::Stale)
    );
}
#[test]
fn final_close_waits_for_operation_pin_and_then_reports_peer_closed() {
    let (mut n, mut o, a, b) = channels();
    let h = endpoint(&n, &o, a, 4);
    let bh = endpoint(&n, &o, b, 4);
    for index in 5..8 {
        let h = endpoint(&n, &o, a, index);
        n.authority_mut(a).unwrap().close(&mut o, a, h).unwrap();
    }
    {
        let t = n.authority_mut(a).unwrap();
        let mut g = t
            .lookup(&mut o, a, h, ObjectType::ChannelEndpoint, Rights::SEND)
            .unwrap();
        g.close_handle(t, h).unwrap();
        assert_eq!(g.info().references, 0);
        assert_eq!(g.info().pins, 1);
        g.channel_send(17).unwrap();
        assert_eq!(g.channel_snapshot()[2] != 0, true);
    }
    assert_eq!(receive(&n, &mut o, b, bh), Ok(17));
    assert_eq!(receive(&n, &mut o, b, bh), Err(Error::PeerClosed));
    assert_eq!(o.channel_state().stats().endpoints, 1);
}
#[test]
fn node_termination_drain_preserves_healthy_peer_pending_messages() {
    let (mut n, mut o, a, b) = channels();
    let ah = endpoint(&n, &o, a, 4);
    let bh = endpoint(&n, &o, b, 4);
    send(&n, &mut o, a, ah, 41).unwrap();
    send(&n, &mut o, b, bh, 42).unwrap();
    n.authority_mut(a).unwrap().terminate();
    assert_eq!(n.authority_mut(a).unwrap().drain(&mut o, a), Ok(4));
    assert_eq!(receive(&n, &mut o, b, bh), Ok(41));
    assert_eq!(receive(&n, &mut o, b, bh), Err(Error::PeerClosed));
    assert_eq!(o.channel_state().snapshot()[20], 1);
    n.authority_mut(b).unwrap().drain(&mut o, b).unwrap();
    assert_eq!(o.stats(), Stats::default());
    assert_eq!(o.channel_state().stats(), crate::channel::Stats::default());
}
#[test]
fn endpoint_constructor_object_exhaustion_never_publishes_half_a_channel() {
    let (mut n, a, b) = owners();
    let mut o = Objects::with_channels();
    let refs: [_; 5] = core::array::from_fn(|_| o.flag(true).unwrap());
    let before = o.stats();
    let snap = o.channel_state().snapshot();
    let gens = o.slots.each_ref().map(|s| s.generation);
    assert_eq!(n.bootstrap_channel(&mut o, a, b), Err(Error::Capacity));
    assert_eq!(o.stats(), before);
    assert_eq!(o.channel_state().snapshot(), snap);
    assert_eq!(o.slots.each_ref().map(|s| s.generation), gens);
    assert!(n.authority(a).unwrap().is_empty());
    assert!(n.authority(b).unwrap().is_empty());
    for r in refs {
        o.release(r).unwrap();
    }
}
#[test]
fn endpoint_constructor_table_exhaustion_and_sealed_manifest_are_atomic() {
    for sealed in [false, true] {
        let (mut n, a, b) = owners();
        let mut o = Objects::with_channels();
        let r = o.flag(true).unwrap();
        if sealed {
            n.authority_mut(b).unwrap().seal_bootstrap();
        } else {
            for _ in 0..5 {
                n.authority_mut(b)
                    .unwrap()
                    .grant(&mut o, &r, Rights::READ)
                    .unwrap();
            }
        }
        let before = o.stats();
        let snap = o.channel_state().snapshot();
        assert_eq!(
            n.bootstrap_channel(&mut o, a, b),
            Err(if sealed {
                Error::InUse
            } else {
                Error::Capacity
            })
        );
        assert_eq!(o.stats(), before);
        assert_eq!(o.channel_state().snapshot(), snap);
        assert!(n.authority(a).unwrap().is_empty());
        assert_eq!(n.authority(a).unwrap().bootstrap_limit, 4);
    }
}
#[test]
fn channel_profile_preserves_legacy_pool_limit_and_original_manifest_range() {
    let mut o = Objects::new();
    let refs: [_; 4] = core::array::from_fn(|_| o.flag(true).unwrap());
    assert_eq!(o.flag(false).err(), Some(Error::Capacity));
    for r in refs {
        o.release(r).unwrap();
    }
    let (n, o, a, _) = channels();
    assert_eq!(
        n.authority(a).unwrap().bootstrap_get(&o, a, 3),
        Err(Error::Handle)
    );
    assert_eq!(
        n.authority(a).unwrap().bootstrap_get(&o, a, 8),
        Err(Error::Handle)
    );
}
#[test]
fn retired_endpoint_object_generations_and_handles_cannot_reach_a_new_pair() {
    let (mut n, mut o, a, b) = channels();
    let old = endpoint(&n, &o, a, 4);
    let id = n.authority(a).unwrap().describe(a, old).unwrap().0;
    for node in [a, b] {
        n.authority_mut(node).unwrap().drain(&mut o, node).unwrap();
    }
    let mut fresh_a = Table::new(a);
    let mut fresh_b = Table::new(b);
    o.install_channel(&mut fresh_a, &mut fresh_b).unwrap();
    fresh_a.seal_bootstrap();
    fresh_b.seal_bootstrap();
    let h = fresh_a.bootstrap_get(&o, a, 4).unwrap();
    fresh_a
        .lookup(&mut o, a, h, ObjectType::ChannelEndpoint, Rights::SEND)
        .unwrap()
        .channel_send(9)
        .unwrap();
    assert_eq!(o.info(id), Err(Error::Object));
    assert_eq!(send(&n, &mut o, a, old, 1), Err(Error::Stale));
}
#[test]
fn endpoint_grants_reject_other_node_owner_and_inappropriate_rights_atomically() {
    let (mut n, a, b) = owners();
    let mut o = Objects::with_channels();
    n.bootstrap_channel(&mut o, a, b).unwrap();
    let target = n.authority(a).unwrap().bootstrap[4].unwrap();
    let id = n.authority(a).unwrap().describe(a, target).unwrap().0;
    o.retain(id).unwrap();
    let creator = ObjectRef { id };
    let before = o.stats();
    assert_eq!(
        n.authority_mut(b)
            .unwrap()
            .grant(&mut o, &creator, Rights::SEND),
        Err(Error::Owner)
    );
    for rights in [
        Rights::NONE,
        Rights::READ,
        Rights::WRITE,
        Rights::INSPECT,
        Rights::from_bits(48).unwrap(),
    ] {
        assert_eq!(
            n.authority_mut(a).unwrap().grant(&mut o, &creator, rights),
            Err(Error::Rights)
        );
    }
    assert_eq!(o.stats(), before);
    o.release(creator).unwrap();
}
#[test]
fn exhausted_endpoint_object_generation_cannot_partially_create_a_pair() {
    let (mut n, a, b) = owners();
    let mut o = Objects::with_channels();
    let _refs: [_; 4] = core::array::from_fn(|_| o.flag(true).unwrap());
    o.slots[4].generation = u32::MAX;
    let before = o.stats();
    let snapshot = o.channel_state().snapshot();
    assert_eq!(n.bootstrap_channel(&mut o, a, b), Err(Error::Capacity));
    assert_eq!(o.stats(), before);
    assert_eq!(o.channel_state().snapshot(), snapshot);
    assert!(n.authority(a).unwrap().is_empty());
}
#[test]
fn full_real_address_space_profile_coexists_with_channel_and_releases_every_resource() {
    let (mut n, a, b) = owners();
    let mut r = Registry::new();
    let mut o = Objects::with_channels();
    let mut spaces = [crate::paging::SpaceId::KERNEL; 2];
    for (i, node) in [a, b].into_iter().enumerate() {
        let space = r.create_space().unwrap();
        r.bind_root(space, 0x1000 * (i as u64 + 1)).unwrap();
        spaces[i] = space;
        n.configure(node).unwrap();
        n.bind_space(node, space).unwrap();
        let creator = o
            .address_space(node, r.retain_space(space).unwrap())
            .unwrap();
        let flag = o.flag(true).unwrap();
        let t = n.authority_mut(node).unwrap();
        for (index, rights) in [
            (0, Rights::INSPECT),
            (1, Rights::INSPECT),
            (2, Rights::NONE),
        ] {
            t.install_bootstrap(&mut o, &creator, rights, index)
                .unwrap();
        }
        t.install_bootstrap(&mut o, &flag, Rights::READ, 3).unwrap();
        o.release(creator).unwrap();
        o.release(flag).unwrap();
    }
    n.bootstrap_channel(&mut o, a, b).unwrap();
    assert_eq!(
        o.stats(),
        Stats {
            live: 6,
            references: 16,
            pins: 0
        }
    );
    assert_eq!(o.flag(false).err(), Some(Error::Capacity));
    for node in [a, b] {
        assert_eq!(n.authority(node).unwrap().len(), 8);
        n.authority_mut(node).unwrap().seal_bootstrap();
        n.authority_mut(node).unwrap().drain(&mut o, node).unwrap();
    }
    while let Some(reference) = o.reap_space() {
        r.release_space_ref(reference).unwrap();
    }
    assert_eq!(o.stats(), Stats::default());
    assert_eq!(o.channel_state().stats(), crate::channel::Stats::default());
    for s in spaces {
        assert_eq!(r.object_references(s), Ok(0));
        assert_eq!(r.can_destroy_space(s), Ok(()));
    }
}
#[test]
fn bootstrap_channel_refuses_to_touch_a_running_nodes_execution_lease() {
    let (mut n, _, mut q, ids) = runnable();
    let a = n.thread(ids[0]).unwrap().node;
    let b = n.thread(ids[2]).unwrap().node;
    let mut o = Objects::with_channels();
    let active = q.next(&mut n).unwrap().unwrap();
    let lease = n.take_authority(active.active).unwrap();
    let before = o.stats();
    assert_eq!(n.bootstrap_channel(&mut o, a, b), Err(Error::InUse));
    assert_eq!(o.stats(), before);
    n.restore_authority(lease).unwrap();
}
#[test]
fn channel_pool_exhaustion_preserves_existing_queued_payload_and_tables() {
    let (n, mut o, a, b) = channels();
    let h = endpoint(&n, &o, a, 4);
    send(&n, &mut o, a, h, 31).unwrap();
    let before = o.stats();
    let snapshot = o.channel_state().snapshot();
    let mut ta = Table::new(a);
    let mut tb = Table::new(b);
    assert_eq!(o.install_channel(&mut ta, &mut tb), Err(Error::Capacity));
    assert_eq!(o.stats(), before);
    assert_eq!(o.channel_state().snapshot(), snapshot);
    assert!(ta.is_empty() && tb.is_empty());
    assert_eq!(receive(&n, &mut o, b, h), Ok(31));
}
#[test]
fn exhausted_handle_generations_cannot_partially_extend_the_manifest() {
    let (mut n, a, b) = owners();
    let mut o = Objects::with_channels();
    for s in &mut n.authority_mut(a).unwrap().slots[..5] {
        s.generation = u32::MAX;
    }
    let before = o.stats();
    let snapshot = o.channel_state().snapshot();
    assert_eq!(n.bootstrap_channel(&mut o, a, b), Err(Error::Capacity));
    assert_eq!(o.stats(), before);
    assert_eq!(o.channel_state().snapshot(), snapshot);
    assert_eq!(n.authority(a).unwrap().bootstrap_limit, 4);
}
#[test]
fn checked_guard_never_amplifies_combined_capability_beyond_requested_rights() {
    let (n, mut o, a, _) = channels();
    let h = endpoint(&n, &o, a, 4);
    let before = o.channel_state().snapshot();
    {
        let mut g = n
            .authority(a)
            .unwrap()
            .lookup(&mut o, a, h, ObjectType::ChannelEndpoint, Rights::SEND)
            .unwrap();
        assert_eq!(g.channel_receive(), Err(Error::Rights));
    }
    {
        let mut g = n
            .authority(a)
            .unwrap()
            .lookup(&mut o, a, h, ObjectType::ChannelEndpoint, Rights::RECEIVE)
            .unwrap();
        assert_eq!(g.channel_send(9), Err(Error::Rights));
    }
    assert_eq!(o.channel_state().snapshot(), before);
    assert_eq!(o.stats().pins, 0);
}
#[test]
fn opposite_node_table_cannot_close_another_tables_entry() {
    let (mut n, mut o, a, b) = channels();
    let h = endpoint(&n, &o, a, 4);
    let before = o.stats();
    assert_eq!(
        n.authority_mut(a).unwrap().close(&mut o, b, h),
        Err(Error::Owner)
    );
    assert_eq!(o.stats(), before);
    send(&n, &mut o, a, h, 32).unwrap();
    assert_eq!(receive(&n, &mut o, b, h), Ok(32));
}

#[test]
fn pair_preflight_refuses_each_owner_capacity_without_consuming_any_generation() {
    for blocked in 0..2 {
        let (mut n, a, b) = owners();
        let mut o = Objects::with_channels();
        let creator = o.flag(true).unwrap();
        let node = [a, b][blocked];
        for _ in 0..5 {
            n.authority_mut(node)
                .unwrap()
                .grant(&mut o, &creator, Rights::READ)
                .unwrap();
        }
        let before = o.stats();
        let snapshot = o.channel_state().snapshot();
        let objects = o.slots.each_ref().map(|slot| slot.generation);
        let generations = [a, b].map(|id| {
            n.authority(id)
                .unwrap()
                .slots
                .each_ref()
                .map(|slot| slot.generation)
        });
        assert_eq!(n.bootstrap_channel(&mut o, a, b), Err(Error::Capacity));
        assert_eq!(o.stats(), before);
        assert_eq!(o.channel_state().snapshot(), snapshot);
        assert_eq!(o.slots.each_ref().map(|slot| slot.generation), objects);
        assert_eq!(
            [a, b].map(|id| n
                .authority(id)
                .unwrap()
                .slots
                .each_ref()
                .map(|slot| slot.generation)),
            generations
        );
        assert_eq!(o.info(creator.id()).unwrap().references, 6);
        n.authority_mut(node).unwrap().drain(&mut o, node).unwrap();
        o.release(creator).unwrap();
        assert_eq!(o.stats(), Stats::default());
    }
}

#[test]
fn each_faulted_owner_discards_only_its_inbound_and_preserves_peer_fifo() {
    for dead_side in 0..2 {
        let (mut n, mut o, a, b) = channels();
        let nodes = [a, b];
        let handles = nodes.map(|id| endpoint(&n, &o, id, 4));
        for side in 0..2 {
            send(&n, &mut o, nodes[side], handles[side], 10 + side as u64).unwrap();
            send(&n, &mut o, nodes[side], handles[side], 20 + side as u64).unwrap();
        }
        n.authority_mut(nodes[dead_side]).unwrap().terminate();
        n.authority_mut(nodes[dead_side])
            .unwrap()
            .drain(&mut o, nodes[dead_side])
            .unwrap();
        let live = 1 - dead_side;
        assert_eq!(o.channel_state().snapshot()[20 + dead_side], 2);
        assert_eq!(
            receive(&n, &mut o, nodes[live], handles[live]),
            Ok(10 + dead_side as u64)
        );
        assert_eq!(
            receive(&n, &mut o, nodes[live], handles[live]),
            Ok(20 + dead_side as u64)
        );
        assert_eq!(
            receive(&n, &mut o, nodes[live], handles[live]),
            Err(Error::PeerClosed)
        );
        assert_eq!(
            send(&n, &mut o, nodes[live], handles[live], 99),
            Err(Error::PeerClosed)
        );
        n.authority_mut(nodes[live])
            .unwrap()
            .drain(&mut o, nodes[live])
            .unwrap();
        assert_eq!(o.stats(), Stats::default());
        assert_eq!(o.channel_state().stats(), crate::channel::Stats::default());
    }
}

/// ADR 0018's per-direction invariant, read from the production registry.
fn assert_channel_conservation(o: &mut Objects) {
    let s = o.channel_state().snapshot();
    let queued = [s[8], s[13]];
    let sent = [s[16], s[17]];
    let received = [s[18], s[19]];
    let discarded = [s[20], s[21]];
    for side in 0..2 {
        assert_eq!(
            sent[1 - side],
            received[side] + queued[side] + discarded[side],
            "direction {side} conservation"
        );
    }
}

fn fault_owner(n: &mut Nodes, o: &mut Objects, dead: NodeId) {
    n.authority_mut(dead).unwrap().terminate();
    n.authority_mut(dead).unwrap().drain(o, dead).unwrap();
}

fn drain_peer_after_fault(
    n: &mut Nodes,
    o: &mut Objects,
    live: NodeId,
    handle: Handle,
    expected: &[u64],
) {
    for value in expected {
        assert_eq!(receive(n, o, live, handle), Ok(*value));
    }
    assert_eq!(receive(n, o, live, handle), Err(Error::PeerClosed));
    assert_eq!(send(n, o, live, handle, 0xdead), Err(Error::PeerClosed));
    n.authority_mut(live).unwrap().drain(o, live).unwrap();
    assert_eq!(o.stats(), Stats::default());
    assert_eq!(o.channel_state().stats(), crate::channel::Stats::default());
}

#[test]
fn fault_timing_matrix_preserves_conservation_and_restores_accounting() {
    for dead_side in 0..2 {
        let dead_side = dead_side as usize;
        // Fault before any committed message.
        {
            let (mut n, mut o, a, b) = channels();
            let nodes = [a, b];
            fault_owner(&mut n, &mut o, nodes[dead_side]);
            assert_channel_conservation(&mut o);
            let live = 1 - dead_side;
            let live_handle = endpoint(&n, &o, nodes[live], 4);
            drain_peer_after_fault(&mut n, &mut o, nodes[live], live_handle, &[]);
        }
        // Fault after one successful commit; live peer drains it first.
        {
            let (mut n, mut o, a, b) = channels();
            let nodes = [a, b];
            let dead_handle = endpoint(&n, &o, nodes[dead_side], 4);
            let live_handle = endpoint(&n, &o, nodes[1 - dead_side], 4);
            send(&n, &mut o, nodes[dead_side], dead_handle, 0x51).unwrap();
            assert_channel_conservation(&mut o);
            fault_owner(&mut n, &mut o, nodes[dead_side]);
            assert_channel_conservation(&mut o);
            drain_peer_after_fault(&mut n, &mut o, nodes[1 - dead_side], live_handle, &[0x51]);
        }
        // Fault after a Full rejection with a full peer FIFO.
        {
            let (mut n, mut o, a, b) = channels();
            let nodes = [a, b];
            let dead_handle = endpoint(&n, &o, nodes[dead_side], 4);
            let live_handle = endpoint(&n, &o, nodes[1 - dead_side], 4);
            send(&n, &mut o, nodes[dead_side], dead_handle, 0x61).unwrap();
            send(&n, &mut o, nodes[dead_side], dead_handle, 0x62).unwrap();
            assert_eq!(
                send(&n, &mut o, nodes[dead_side], dead_handle, 0x63),
                Err(Error::Full)
            );
            assert_channel_conservation(&mut o);
            fault_owner(&mut n, &mut o, nodes[dead_side]);
            assert_channel_conservation(&mut o);
            drain_peer_after_fault(
                &mut n,
                &mut o,
                nodes[1 - dead_side],
                live_handle,
                &[0x61, 0x62],
            );
        }
        // Fault after a successful receive, with messages pending both ways.
        {
            let (mut n, mut o, a, b) = channels();
            let nodes = [a, b];
            let dead_send = endpoint(&n, &o, nodes[dead_side], 4);
            let dead_recv = endpoint(&n, &o, nodes[dead_side], 5);
            let live_send = endpoint(&n, &o, nodes[1 - dead_side], 4);
            let live_recv = endpoint(&n, &o, nodes[1 - dead_side], 5);
            send(&n, &mut o, nodes[dead_side], dead_send, 0x71).unwrap();
            send(&n, &mut o, nodes[1 - dead_side], live_send, 0x72).unwrap();
            assert_eq!(receive(&n, &mut o, nodes[dead_side], dead_recv), Ok(0x72));
            assert_channel_conservation(&mut o);
            fault_owner(&mut n, &mut o, nodes[dead_side]);
            assert_channel_conservation(&mut o);
            drain_peer_after_fault(&mut n, &mut o, nodes[1 - dead_side], live_recv, &[0x71]);
        }
        // Fault after closing one of several endpoint references.
        {
            let (mut n, mut o, a, b) = channels();
            let nodes = [a, b];
            let first = endpoint(&n, &o, nodes[dead_side], 4);
            let second = endpoint(&n, &o, nodes[dead_side], 6);
            let live_handle = endpoint(&n, &o, nodes[1 - dead_side], 4);
            n.authority_mut(nodes[dead_side])
                .unwrap()
                .close(&mut o, nodes[dead_side], first)
                .unwrap();
            assert_eq!(
                send(&n, &mut o, nodes[dead_side], first, 0x81),
                Err(Error::Stale)
            );
            send(&n, &mut o, nodes[dead_side], second, 0x82).unwrap();
            assert_channel_conservation(&mut o);
            fault_owner(&mut n, &mut o, nodes[dead_side]);
            assert_channel_conservation(&mut o);
            drain_peer_after_fault(&mut n, &mut o, nodes[1 - dead_side], live_handle, &[0x82]);
        }
    }
}

#[test]
fn blocking_receive_after_a_committed_send_never_registers_a_wait() {
    let (n, mut o, a1, _, b1) = waiters();
    let ah = endpoint(&n, &o, a1.node, 7);
    let bh = endpoint(&n, &o, b1.node, 6);
    send(&n, &mut o, b1.node, bh, 0x1111).unwrap();
    assert_eq!(
        receive_start(&n, &mut o, a1, ah),
        Ok(WaitStart::Committed(0x1111))
    );
    assert!(o.wait_stats() == crate::wait::Stats::default());
    assert_eq!(o.stats().pins, 0);
}

#[test]
fn blocked_send_is_rechecked_and_commits_once_capacity_exists() {
    let (n, mut o, a1, _, b1) = waiters();
    let ah = endpoint(&n, &o, a1.node, 4);
    let bh = endpoint(&n, &o, b1.node, 4);
    send(&n, &mut o, a1.node, ah, 1).unwrap();
    send(&n, &mut o, a1.node, ah, 2).unwrap();
    assert_eq!(send_start(&n, &mut o, a1, ah, 3), Ok(WaitStart::Blocked));
    assert_eq!(o.channel_state().snapshot()[13], 2);
    assert_eq!(o.wait_stats().waiting, 1);
    assert_eq!(o.stats().pins, 1);
    assert_eq!(receive(&n, &mut o, b1.node, bh), Ok(1));
    let (ready, count) = queued(&mut o);
    assert_eq!(count, 1);
    let waiter = ready[0].unwrap();
    assert_eq!(waiter.kind, crate::wait::Kind::Send);
    assert_eq!(waiter.payload, 3);
    assert_eq!(o.settle_wait(a1.thread), Settle::Committed(0));
    assert_eq!(receive(&n, &mut o, b1.node, bh), Ok(2));
    assert_eq!(receive(&n, &mut o, b1.node, bh), Ok(3));
    assert_eq!(o.channel_state().snapshot()[16..22], [3, 0, 0, 3, 0, 0]);
    assert_eq!(o.stats().pins, 0);
}

#[test]
fn woken_receiver_that_finds_the_queue_empty_is_restored_without_losing_its_pin() {
    let (n, mut o, a1, a2, b1) = waiters();
    let ah = endpoint(&n, &o, a1.node, 7);
    let bh = endpoint(&n, &o, b1.node, 6);
    assert_eq!(receive_start(&n, &mut o, a1, ah), Ok(WaitStart::Blocked));
    send(&n, &mut o, b1.node, bh, 10).unwrap();
    assert_eq!(o.wait_stats().ready, 1);
    assert_eq!(queued(&mut o).1, 1);
    // A different Thread on the same Node consumes the only message before the
    // woken Thread reaches its recheck.
    assert_eq!(receive(&n, &mut o, a2.node, ah), Ok(10));
    assert_eq!(o.settle_wait(a1.thread), Settle::Retry);
    let waiter = o.wait_waiter(a1.thread).unwrap();
    assert_eq!(waiter.state, crate::wait::State::Waiting);
    assert!(waiter.pinned);
    assert_eq!(o.stats().pins, 1);
    send(&n, &mut o, b1.node, bh, 11).unwrap();
    assert_eq!(queued(&mut o).1, 1);
    assert_eq!(o.settle_wait(a1.thread), Settle::Committed(11));
    assert_eq!(o.stats().pins, 0);
}

#[test]
fn woken_sender_that_finds_the_queue_full_is_restored_without_duplicate_pin() {
    let (n, mut o, a1, a2, b1) = waiters();
    let ah = endpoint(&n, &o, a1.node, 4);
    let bh = endpoint(&n, &o, b1.node, 4);
    send(&n, &mut o, a1.node, ah, 1).unwrap();
    send(&n, &mut o, a1.node, ah, 2).unwrap();
    assert_eq!(send_start(&n, &mut o, a1, ah, 3), Ok(WaitStart::Blocked));
    assert_eq!(receive(&n, &mut o, b1.node, bh), Ok(1));
    // A2 refills the receiver FIFO before A1's recheck.
    send(&n, &mut o, a2.node, ah, 4).unwrap();
    assert_eq!(queued(&mut o).1, 1);
    assert_eq!(o.settle_wait(a1.thread), Settle::Retry);
    assert!(o.wait_waiter(a1.thread).unwrap().pinned);
    assert_eq!(o.stats().pins, 1);
    assert_eq!(receive(&n, &mut o, b1.node, bh), Ok(2));
    assert_eq!(queued(&mut o).1, 1);
    assert_eq!(o.settle_wait(a1.thread), Settle::Committed(0));
    assert_eq!(receive(&n, &mut o, b1.node, bh), Ok(4));
    assert_eq!(receive(&n, &mut o, b1.node, bh), Ok(3));
    assert_eq!(o.stats().pins, 0);
}

#[test]
fn blocked_send_preserves_the_full_width_payload_until_commit() {
    let (n, mut o, a1, _, b1) = waiters();
    let ah = endpoint(&n, &o, a1.node, 4);
    let bh = endpoint(&n, &o, b1.node, 4);
    send(&n, &mut o, a1.node, ah, 0).unwrap();
    send(&n, &mut o, a1.node, ah, 1).unwrap();
    assert_eq!(
        send_start(&n, &mut o, a1, ah, u64::MAX),
        Ok(WaitStart::Blocked)
    );
    assert_eq!(o.wait_waiter(a1.thread).unwrap().payload, u64::MAX);
    assert_eq!(receive(&n, &mut o, b1.node, bh), Ok(0));
    assert_eq!(queued(&mut o).1, 1);
    assert_eq!(o.settle_wait(a1.thread), Settle::Committed(0));
    assert_eq!(receive(&n, &mut o, b1.node, bh), Ok(1));
    assert_eq!(receive(&n, &mut o, b1.node, bh), Ok(u64::MAX));
    assert_eq!(o.channel_state().snapshot()[16..22], [3, 0, 0, 3, 0, 0]);
}

#[test]
fn receive_delivers_exactly_one_scalar_and_never_wakes_an_already_ready_waiter_twice() {
    let (n, mut o, a1, a2, b1) = waiters();
    let ah = endpoint(&n, &o, a1.node, 7);
    let bh = endpoint(&n, &o, b1.node, 6);
    assert_eq!(receive_start(&n, &mut o, a1, ah), Ok(WaitStart::Blocked));
    send(&n, &mut o, b1.node, bh, 0xaa).unwrap();
    let token = o.wait_waiter(a1.thread).unwrap().token;
    let stats = o.wait_stats();
    send(&n, &mut o, b1.node, bh, 0xbb).unwrap();
    assert_eq!(o.wait_stats(), stats);
    assert!(!o.waits.mark_ready(token));
    assert_eq!(queued(&mut o).1, 1);
    assert_eq!(o.settle_wait(a1.thread), Settle::Committed(0xaa));
    assert_eq!(receive(&n, &mut o, a2.node, ah), Ok(0xbb));
    assert_eq!(receive(&n, &mut o, a2.node, ah), Err(Error::Empty));
    assert_eq!(o.channel_state().snapshot()[18], 2);
}

#[test]
fn peer_close_wakes_all_affected_waiters_and_reports_peer_closed() {
    let (mut n, mut o, a1, a2, b1) = waiters();
    let ah = endpoint(&n, &o, a1.node, 7);
    assert_eq!(receive_start(&n, &mut o, a1, ah), Ok(WaitStart::Blocked));
    assert_eq!(receive_start(&n, &mut o, a2, ah), Ok(WaitStart::Blocked));
    close_all(&mut n, &mut o, b1.node);
    let stats = o.wait_stats();
    assert_eq!(stats.ready, 2);
    assert_eq!(stats.waiting, 0);
    assert_eq!(o.stats().pins, 2);
    let (ready, count) = queued(&mut o);
    assert_eq!(count, 2);
    assert_eq!(ready[0].unwrap().thread(), a1.thread);
    assert_eq!(ready[1].unwrap().thread(), a2.thread);
    assert_eq!(o.settle_wait(a1.thread), Settle::Failed(Error::PeerClosed));
    assert_eq!(o.settle_wait(a2.thread), Settle::Failed(Error::PeerClosed));
    assert_eq!(o.stats().pins, 0);
    assert_eq!(o.channel_state().stats().endpoints, 1);
}

#[test]
fn closing_the_waited_endpoint_during_an_in_flight_wait_is_safe() {
    let (mut n, mut o, a1, _, b1) = waiters();
    let ah = endpoint(&n, &o, a1.node, 7);
    assert_eq!(receive_start(&n, &mut o, a1, ah), Ok(WaitStart::Blocked));
    for index in 4..7 {
        let h = n
            .authority(a1.node)
            .unwrap()
            .bootstrap_get(&o, a1.node, index)
            .unwrap();
        n.authority_mut(a1.node)
            .unwrap()
            .close(&mut o, a1.node, h)
            .unwrap();
    }
    assert_eq!(o.wait_stats().waiting, 1);
    assert_eq!(o.stats().pins, 1);
    n.authority_mut(a1.node)
        .unwrap()
        .close(&mut o, a1.node, ah)
        .unwrap();
    let stats = o.wait_stats();
    assert_eq!(stats.ready, 1);
    assert_eq!(stats.terminal, 1);
    assert_eq!(o.stats().pins, 0);
    assert_eq!(o.channel_state().stats().endpoints, 1);
    let (_, count) = queued(&mut o);
    assert_eq!(count, 1);
    assert_eq!(o.settle_wait(a1.thread), Settle::Failed(Error::PeerClosed));
    assert!(o.wait_waiter(a1.thread).is_none());
    close_all(&mut n, &mut o, b1.node);
    assert_eq!(o.channel_state().stats(), crate::channel::Stats::default());
}

#[test]
fn node_cancellation_removes_all_owned_waits_and_preserves_a_surviving_peer_pin() {
    let (mut n, mut o, a1, a2, b1) = waiters();
    let ah = endpoint(&n, &o, a1.node, 7);
    let bh = endpoint(&n, &o, b1.node, 7);
    assert_eq!(receive_start(&n, &mut o, a1, ah), Ok(WaitStart::Blocked));
    assert_eq!(receive_start(&n, &mut o, a2, ah), Ok(WaitStart::Blocked));
    assert_eq!(receive_start(&n, &mut o, b1, bh), Ok(WaitStart::Blocked));
    assert_eq!(o.node_wait_count(a1.node), 2);
    assert_eq!(o.node_wait_count(b1.node), 1);
    assert_eq!(o.pins_outside_node(a1.node), 1);
    assert_eq!(o.cancel_node_waits(a1.node), 2);
    assert_eq!(o.node_wait_count(a1.node), 0);
    assert_eq!(o.node_wait_count(b1.node), 1);
    assert_eq!(o.pins_outside_node(a1.node), 1);
    assert_eq!(o.pins_outside_node(b1.node), 0);
    assert_eq!(o.stats().pins, 1);
    assert_eq!(o.cancel_node_waits(b1.node), 1);
    assert_eq!(o.stats().pins, 0);
    close_all(&mut n, &mut o, a1.node);
    close_all(&mut n, &mut o, b1.node);
    assert_eq!(o.stats(), Stats::default());
    assert_eq!(o.channel_state().stats(), crate::channel::Stats::default());
}

#[test]
fn wait_registration_capacity_failure_after_the_pin_rolls_back_cleanly() {
    let (n, mut o, a1, _, _) = waiters();
    let ah = endpoint(&n, &o, a1.node, 7);
    o.waits.force_sequence_exhaustion();
    assert_eq!(receive_start(&n, &mut o, a1, ah), Err(Error::Capacity));
    assert!(o.wait_stats() == crate::wait::Stats::default());
    assert_eq!(o.stats().pins, 0);
    assert!(!o.channel_state().snapshot()[16..18].iter().any(|v| *v != 0));
}

#[test]
fn operation_pin_exhaustion_never_registers_or_suspends() {
    let (n, mut o, a1, _, _) = waiters();
    let ah = endpoint(&n, &o, a1.node, 7);
    let (id, _) = n.authority(a1.node).unwrap().describe(a1.node, ah).unwrap();
    o.slots[id.slot].pins = u32::MAX;
    assert_eq!(receive_start(&n, &mut o, a1, ah), Err(Error::Capacity));
    assert!(o.wait_stats() == crate::wait::Stats::default());
    assert!(o.wait_waiter(a1.thread).is_none());
    o.slots[id.slot].pins = 0;
}

#[test]
fn rights_failure_and_duplicate_registration_happen_before_suspension() {
    let (n, mut o, a1, _, _) = waiters();
    let send_only = endpoint(&n, &o, a1.node, 6);
    let receive_only = endpoint(&n, &o, a1.node, 7);
    assert_eq!(receive_start(&n, &mut o, a1, send_only), Err(Error::Rights));
    assert_eq!(o.stats().pins, 0);
    assert_eq!(
        receive_start(&n, &mut o, a1, receive_only),
        Ok(WaitStart::Blocked)
    );
    let snapshot = o.wait_stats();
    let pins = o.stats().pins;
    assert_eq!(
        receive_start(&n, &mut o, a1, receive_only),
        Err(Error::InUse)
    );
    assert_eq!(o.wait_stats(), snapshot);
    assert_eq!(o.stats().pins, pins);
    let stale = Handle::from_raw(send_only.raw().wrapping_add(1 << 32));
    assert_eq!(receive_start(&n, &mut o, a1, stale), Err(Error::Stale));
    assert_eq!(o.wait_stats(), snapshot);
}

#[test]
fn multiple_receivers_are_woken_in_registration_fifo_order_once_each() {
    let (n, mut o, a1, a2, b1) = waiters();
    let ah = endpoint(&n, &o, a1.node, 7);
    let bh = endpoint(&n, &o, b1.node, 6);
    assert_eq!(receive_start(&n, &mut o, a1, ah), Ok(WaitStart::Blocked));
    assert_eq!(receive_start(&n, &mut o, a2, ah), Ok(WaitStart::Blocked));
    send(&n, &mut o, b1.node, bh, 0xc1).unwrap();
    assert_eq!(o.wait_stats().ready, 1);
    assert_eq!(
        o.wait_waiter(a1.thread).unwrap().state,
        crate::wait::State::Ready
    );
    assert_eq!(
        o.wait_waiter(a2.thread).unwrap().state,
        crate::wait::State::Waiting
    );
    send(&n, &mut o, b1.node, bh, 0xc2).unwrap();
    assert_eq!(o.wait_stats().ready, 2);
    let (ready, count) = queued(&mut o);
    assert_eq!(count, 2);
    assert_eq!(ready[0].unwrap().thread(), a1.thread);
    assert_eq!(ready[1].unwrap().thread(), a2.thread);
    assert_eq!(o.settle_wait(a1.thread), Settle::Committed(0xc1));
    assert_eq!(o.settle_wait(a2.thread), Settle::Committed(0xc2));
    assert_eq!(receive(&n, &mut o, a1.node, ah), Err(Error::Empty));
    assert_eq!(o.channel_state().snapshot()[18], 2);
    assert_eq!(o.stats().pins, 0);
}

#[test]
fn settling_a_wait_before_its_wake_is_rejected_without_touching_the_pin() {
    let (n, mut o, a1, _, b1) = waiters();
    let ah = endpoint(&n, &o, a1.node, 7);
    let bh = endpoint(&n, &o, b1.node, 6);
    assert_eq!(receive_start(&n, &mut o, a1, ah), Ok(WaitStart::Blocked));
    assert_eq!(o.settle_wait(a1.thread), Settle::Failed(Error::Stale));
    assert_eq!(o.wait_stats().waiting, 1);
    assert_eq!(o.stats().pins, 1);
    send(&n, &mut o, b1.node, bh, 0x51).unwrap();
    assert_eq!(queued(&mut o).1, 1);
    assert_eq!(o.settle_wait(a1.thread), Settle::Committed(0x51));
    assert_eq!(o.stats().pins, 0);
}

#[test]
fn cancelled_wait_tokens_cannot_be_settled_or_revived_into_a_new_channel() {
    let (mut n, mut o, a1, a2, b1) = waiters();
    let ah = endpoint(&n, &o, a1.node, 7);
    let bh = endpoint(&n, &o, b1.node, 6);
    assert_eq!(receive_start(&n, &mut o, a1, ah), Ok(WaitStart::Blocked));
    assert_eq!(receive_start(&n, &mut o, a2, ah), Ok(WaitStart::Blocked));
    let old = o.wait_waiter(a1.thread).unwrap().token;
    assert_eq!(o.cancel_node_waits(a1.node), 2);
    assert!(o.take_ready_waits().into_iter().all(|w| w.is_none()));
    assert_eq!(o.settle_wait(a1.thread), Settle::Failed(Error::Object));
    assert!(o.wait_waiter(a1.thread).is_none());
    assert_eq!(o.stats().pins, 0);
    send(&n, &mut o, b1.node, bh, 1).unwrap();
    assert_eq!(receive(&n, &mut o, a1.node, ah), Ok(1));
    assert!(o.wait_waiter(a1.thread).is_none());
    assert!(o.wait_snapshot()[6..].iter().all(|w| *w == 0));
    let _ = old;
}

#[test]
fn retry_preserves_registration_fifo_for_the_next_wake() {
    let (n, mut o, a1, a2, b1) = waiters();
    let ah = endpoint(&n, &o, a1.node, 7);
    let bh = endpoint(&n, &o, b1.node, 6);
    assert_eq!(receive_start(&n, &mut o, a1, ah), Ok(WaitStart::Blocked));
    assert_eq!(receive_start(&n, &mut o, a2, ah), Ok(WaitStart::Blocked));
    send(&n, &mut o, b1.node, bh, 1).unwrap();
    assert_eq!(queued(&mut o).1, 1);
    assert_eq!(receive(&n, &mut o, a2.node, ah), Ok(1));
    assert_eq!(o.settle_wait(a1.thread), Settle::Retry);
    send(&n, &mut o, b1.node, bh, 2).unwrap();
    let waiter = o.take_ready_waits();
    assert_eq!(waiter[0].unwrap().thread(), a1.thread);
    assert!(waiter[1].is_none());
    assert_eq!(o.settle_wait(a1.thread), Settle::Committed(2));
    send(&n, &mut o, b1.node, bh, 3).unwrap();
    assert_eq!(o.take_ready_waits()[0].unwrap().thread(), a2.thread);
    assert_eq!(o.settle_wait(a2.thread), Settle::Committed(3));
    assert_eq!(o.stats().pins, 0);
}

#[test]
fn a_woken_receiver_drains_a_committed_message_before_observing_peer_close() {
    let (mut n, mut o, a1, _, b1) = waiters();
    let ah = endpoint(&n, &o, a1.node, 6);
    let bh = endpoint(&n, &o, b1.node, 7);
    assert_eq!(receive_start(&n, &mut o, b1, bh), Ok(WaitStart::Blocked));
    send(&n, &mut o, a1.node, ah, 0x77).unwrap();
    assert_eq!(queued(&mut o).1, 1);
    // The sending peer retires while the woken receiver has not yet been scheduled.
    close_all(&mut n, &mut o, a1.node);
    let stats = o.wait_stats();
    assert_eq!(stats.queued, 1);
    assert_eq!(stats.terminal, 0);
    assert_eq!(o.settle_wait(b1.thread), Settle::Committed(0x77));
    assert_eq!(receive(&n, &mut o, b1.node, bh), Err(Error::PeerClosed));
    assert_eq!(o.stats().pins, 0);
    close_all(&mut n, &mut o, b1.node);
    assert_eq!(o.channel_state().stats(), crate::channel::Stats::default());
}

#[test]
fn terminalizing_a_queued_waiter_does_not_admit_the_thread_twice() {
    let (mut n, mut o, a1, _, b1) = waiters();
    let ah = endpoint(&n, &o, a1.node, 7);
    let bh = endpoint(&n, &o, b1.node, 6);

    assert_eq!(receive_start(&n, &mut o, a1, ah), Ok(WaitStart::Blocked));
    let token = o.wait_waiter(a1.thread).unwrap().token;
    send(&n, &mut o, b1.node, bh, 0x71).unwrap();

    // This is the controller's take-ready boundary: the waiter is now already
    // admitted to its run queue, but remains in the registry until settlement.
    let (ready, count) = queued(&mut o);
    assert_eq!(count, 1);
    assert_eq!(ready[0].unwrap().thread(), a1.thread);
    assert_eq!(
        o.wait_waiter(a1.thread).unwrap().state,
        crate::wait::State::Queued
    );

    // Retiring the waited endpoint terminalizes the outstanding registration.
    // It must not downgrade Queued to Ready, or the next drain would enqueue
    // the same Thread a second time.
    close_all(&mut n, &mut o, a1.node);
    let terminal = o.wait_waiter(a1.thread).unwrap();
    assert_eq!(terminal.token, token);
    assert_eq!(terminal.state, crate::wait::State::Queued);
    assert_eq!(terminal.terminal, Some(Error::PeerClosed));
    assert_eq!(o.wait_stats().queued, 1);
    assert_eq!(o.wait_stats().ready, 0);
    assert_eq!(queued(&mut o).1, 0);

    assert_eq!(o.settle_wait(a1.thread), Settle::Failed(Error::PeerClosed));
    assert!(o.wait_waiter(a1.thread).is_none());
    assert_eq!(o.stats().pins, 0);
}

fn movable_wait_fixture() -> (
    Nodes,
    Objects,
    ActiveContext,
    ActiveContext,
    ActiveContext,
    Handle,
) {
    let (mut n, mut o, a1, a2, b1) = waiters();
    let endpoint = endpoint(&n, &o, a1.node, 4);
    let flag = o.flag(true).unwrap();
    let table = n.authority_mut(a1.node).unwrap();
    table.sealed = false;
    let source = table.grant_movable(&mut o, &flag, Rights::READ).unwrap();
    table.slots[endpoint.parts().unwrap().0]
        .entry
        .as_mut()
        .unwrap()
        .rights = Rights(224);
    table.seal_bootstrap();
    o.release(flag).unwrap();
    (n, o, a1, a2, b1, source)
}

#[test]
fn capability_enqueue_wakes_scalar_waiter_to_kind_error_without_consuming_authority() {
    let (mut n, mut o, a, _, b, source) = movable_wait_fixture();
    let ah = endpoint(&n, &o, a.node, 4);
    let bh = endpoint(&n, &o, b.node, 4);
    assert_eq!(receive_start(&n, &mut o, b, bh), Ok(WaitStart::Blocked));
    n.authority_mut(a.node)
        .unwrap()
        .send_cap(&mut o, a.node, ah, source)
        .unwrap();
    assert_eq!(queued(&mut o).1, 1);
    let before = o.channel_state().snapshot();
    let tags = o.channel_state().transfer_snapshot();
    assert_eq!(o.settle_wait(b.thread), Settle::Failed(Error::MessageKind));
    assert_eq!(o.stats().pins, 0);
    assert_eq!(o.channel_state().snapshot(), before);
    assert_eq!(o.channel_state().transfer_snapshot(), tags);
    let received = n
        .authority_mut(b.node)
        .unwrap()
        .receive_cap(&mut o, b.node, bh)
        .unwrap();
    assert_eq!(
        n.authority(b.node)
            .unwrap()
            .lookup(&mut o, b.node, received, ObjectType::Flag, Rights::READ)
            .unwrap()
            .read(),
        Ok(1)
    );
    n.authority_mut(b.node)
        .unwrap()
        .close(&mut o, b.node, received)
        .unwrap();
    close_all(&mut n, &mut o, a.node);
    close_all(&mut n, &mut o, b.node);
    assert_eq!(o.stats(), Stats::default());
}

#[test]
fn capability_dequeue_wakes_full_scalar_sender_and_settlement_commits_once() {
    let (mut n, mut o, a, a2, b, source) = movable_wait_fixture();
    let ah = endpoint(&n, &o, a.node, 4);
    let bh = endpoint(&n, &o, b.node, 4);
    n.authority_mut(a.node)
        .unwrap()
        .send_cap(&mut o, a.node, ah, source)
        .unwrap();
    send(&n, &mut o, a.node, ah, 77).unwrap();
    assert_eq!(send_start(&n, &mut o, a2, ah, 88), Ok(WaitStart::Blocked));
    let received = n
        .authority_mut(b.node)
        .unwrap()
        .receive_cap(&mut o, b.node, bh)
        .unwrap();
    assert_eq!(queued(&mut o).1, 1);
    assert_eq!(o.settle_wait(a2.thread), Settle::Committed(0));
    assert_eq!(receive(&n, &mut o, b.node, bh), Ok(77));
    assert_eq!(receive(&n, &mut o, b.node, bh), Ok(88));
    assert_eq!(o.stats().pins, 0);
    n.authority_mut(b.node)
        .unwrap()
        .close(&mut o, b.node, received)
        .unwrap();
    close_all(&mut n, &mut o, a.node);
    close_all(&mut n, &mut o, b.node);
    assert_eq!(o.stats(), Stats::default());
}

#[test]
fn cancellation_of_scalar_wait_and_discard_of_queued_move_release_distinct_pins_and_refs() {
    let (mut n, mut o, a, a2, b, source) = movable_wait_fixture();
    let ah = endpoint(&n, &o, a.node, 4);
    let bh = endpoint(&n, &o, b.node, 4);
    assert_eq!(receive_start(&n, &mut o, b, bh), Ok(WaitStart::Blocked));
    n.authority_mut(a.node)
        .unwrap()
        .send_cap(&mut o, a.node, ah, source)
        .unwrap();
    assert_eq!(receive_start(&n, &mut o, a2, ah), Ok(WaitStart::Blocked));
    assert_eq!(o.cancel_node_waits(b.node), 1);
    close_all(&mut n, &mut o, b.node);
    assert_eq!(o.channel_state().stats().queued, 0);
    assert_eq!(queued(&mut o).1, 1);
    assert_eq!(o.settle_wait(a2.thread), Settle::Failed(Error::PeerClosed));
    assert_eq!(o.stats().pins, 0);
    close_all(&mut n, &mut o, a.node);
    assert_eq!(o.stats(), Stats::default());
}
