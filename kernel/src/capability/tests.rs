use super::*;
use crate::{
    context::{SavedContext, StackBinding},
    node::{self, Nodes, ThreadContext},
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
    let generations = o.slots.map(|s| s.generation);
    assert!(matches!(o.counter(999), Err(Error::Capacity)));
    assert_eq!(o.stats(), before);
    assert_eq!(o.slots.map(|s| s.generation), generations);
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
